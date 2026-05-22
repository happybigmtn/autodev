//! Tracked-file enumeration, crate/group classification, per-file artifact
//! addressing, and initial group-report assembly.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use crate::audit_everything::manifest::{EverythingManifest, FileState, GroupState, StageStatus};
use crate::audit_everything::run_paths::RunPaths;
use crate::audit_everything::{sha256_hex, short_hash, slugify};
use crate::util::git_stdout;

pub(crate) const MAX_FILE_PROMPT_BYTES: usize = 220_000;
pub(crate) const LEGACY_LARGE_FILE_OMISSION_MARKER: &str = "[file omitted from prompt because";
const DEFAULT_EXCLUDE_PREFIXES: [&str; 16] = [
    ".git/",
    ".auto/",
    ".claude/worktrees/",
    ".gstack/",
    "audit/",
    "target/",
    "node_modules/",
    "dist/",
    "build/",
    "bug/",
    "nemesis/",
    "gen-",
    ".github/ISSUE_TEMPLATE/",
    "docs/ops/operator-evidence/",
    "web/client/dist/",
    "web/play/dist/",
];
const DEFAULT_EXCLUDE_PATH_SEGMENTS: [&str; 3] = ["/node_modules/", "/target/", "/dist/"];
const DEFAULT_EXCLUDE_SUFFIXES: [&str; 12] = [
    ".lock", ".map", ".png", ".jpg", ".jpeg", ".webp", ".gif", ".pdf", ".ico", ".mp4", ".mov",
    ".zip",
];
const DEFAULT_EXCLUDE_FILENAMES: [&str; 4] = [
    "Cargo.lock",
    "pnpm-lock.yaml",
    "package-lock.json",
    "bun.lockb",
];

pub(crate) fn reconcile_file_inventory(
    worktree_root: &Path,
    report_root: &Path,
    manifest: &mut EverythingManifest,
) -> Result<()> {
    if !worktree_root.exists() {
        return Ok(());
    }
    let tracked = enumerate_tracked_files(worktree_root)?;
    let existing_status = manifest
        .files
        .iter()
        .map(|file| (file.path.clone(), file.status))
        .collect::<BTreeMap<_, _>>();
    let groups = classify_groups(worktree_root, &tracked);
    let mut files = Vec::new();
    for path in tracked {
        let absolute_path = worktree_root.join(&path);
        if !absolute_path.is_file() {
            continue;
        }
        let content = fs::read(&absolute_path)
            .with_context(|| format!("failed to read {}", absolute_path.display()))?;
        let hash = sha256_hex(&content);
        let artifact_path = file_artifact_dir(report_root, &path, &hash);
        let legacy_artifact_path = legacy_file_artifact_dir(report_root, &hash);
        migrate_legacy_file_artifact_if_matching(&legacy_artifact_path, &artifact_path, &path)?;
        let artifact_dir = artifact_path.display().to_string();
        let status = if artifact_complete(&artifact_path) {
            StageStatus::Complete
        } else {
            existing_status
                .get(&path)
                .copied()
                .filter(|status| !matches!(status, StageStatus::Complete))
                .unwrap_or(StageStatus::Pending)
        };
        files.push(FileState {
            group: groups
                .get(&path)
                .cloned()
                .unwrap_or_else(|| "root".to_string()),
            path,
            content_hash: hash,
            artifact_dir,
            status,
        });
    }
    files.sort_by(|left, right| left.path.cmp(&right.path));
    manifest.files = files;
    rebuild_group_states(report_root, manifest);
    Ok(())
}

fn enumerate_tracked_files(repo_root: &Path) -> Result<Vec<String>> {
    let listing = git_stdout(repo_root, ["ls-files", "-z"])?;
    let mut files = listing
        .split('\0')
        .filter(|path| !path.is_empty())
        .filter(|path| !excluded_path(path))
        .map(ToOwned::to_owned)
        .collect::<Vec<_>>();
    for context_file in ["AGENTS.md", "ARCHITECTURE.md"] {
        if repo_root.join(context_file).exists() && !files.iter().any(|path| path == context_file) {
            files.push(context_file.to_string());
        }
    }
    files.sort();
    files.dedup();
    Ok(files)
}

pub(crate) fn excluded_path(path: &str) -> bool {
    let path = path.trim_start_matches("./");
    if DEFAULT_EXCLUDE_FILENAMES
        .iter()
        .any(|filename| path == *filename || path.ends_with(&format!("/{filename}")))
    {
        return true;
    }
    if DEFAULT_EXCLUDE_SUFFIXES
        .iter()
        .any(|suffix| path.to_ascii_lowercase().ends_with(suffix))
    {
        return true;
    }
    if DEFAULT_EXCLUDE_PATH_SEGMENTS
        .iter()
        .any(|segment| path.contains(segment))
    {
        return true;
    }
    DEFAULT_EXCLUDE_PREFIXES.iter().any(|prefix| {
        if prefix.ends_with('/') {
            path.starts_with(prefix)
        } else {
            path == *prefix || path.starts_with(prefix)
        }
    })
}

fn classify_groups(repo_root: &Path, files: &[String]) -> BTreeMap<String, String> {
    let crate_roots = cargo_member_roots(repo_root);
    let mut map = BTreeMap::new();
    for path in files {
        let group = crate_roots
            .iter()
            .filter(|root| path == *root || path.starts_with(&format!("{root}/")))
            .max_by_key(|root| root.len())
            .cloned()
            .unwrap_or_else(|| fallback_group(path));
        map.insert(path.clone(), group);
    }
    map
}

fn cargo_member_roots(repo_root: &Path) -> Vec<String> {
    let mut roots = BTreeSet::new();
    let cargo = repo_root.join("Cargo.toml");
    if let Ok(raw) = fs::read_to_string(&cargo) {
        if let Ok(value) = raw.parse::<toml::Value>() {
            if value
                .get("package")
                .and_then(|pkg| pkg.get("name"))
                .is_some()
            {
                roots.insert(".".to_string());
            }
            if let Some(members) = value
                .get("workspace")
                .and_then(|workspace| workspace.get("members"))
                .and_then(|members| members.as_array())
            {
                for member in members.iter().filter_map(|member| member.as_str()) {
                    if !member.contains('*') {
                        roots.insert(member.trim_matches('/').to_string());
                    }
                }
            }
        }
    }
    roots.into_iter().collect()
}

pub(crate) fn fallback_group(path: &str) -> String {
    if path.starts_with("crates/") {
        return path.split('/').take(2).collect::<Vec<_>>().join("/");
    }
    if path.starts_with("src/") {
        return "src".to_string();
    }
    if path.starts_with("tests/") {
        return "tests".to_string();
    }
    if path.starts_with("docs/") {
        return "docs".to_string();
    }
    if path.starts_with("specs/") {
        return "specs".to_string();
    }
    path.split('/').next().unwrap_or("root").to_string()
}

fn rebuild_group_states(report_root: &Path, manifest: &mut EverythingManifest) {
    let old = manifest
        .groups
        .iter()
        .map(|group| (group.name.clone(), group.clone()))
        .collect::<BTreeMap<_, _>>();
    let mut grouped: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for file in &manifest.files {
        grouped
            .entry(file.group.clone())
            .or_default()
            .push(file.path.clone());
    }
    manifest.groups = grouped
        .into_iter()
        .map(|(name, mut files)| {
            files.sort();
            let slug = slugify(&name);
            let report_path = report_root
                .join("reports")
                .join(format!("{slug}.md"))
                .display()
                .to_string();
            let previous = old.get(&name);
            GroupState {
                name,
                slug,
                files,
                report_path,
                synthesis_status: previous
                    .map(|group| group.synthesis_status)
                    .unwrap_or(StageStatus::Pending),
                remediation_status: previous
                    .map(|group| group.remediation_status)
                    .unwrap_or(StageStatus::Pending),
            }
        })
        .collect();
}

pub(crate) fn build_initial_group_reports(
    paths: &RunPaths,
    manifest: &EverythingManifest,
) -> Result<()> {
    fs::create_dir_all(paths.report_root.join("reports")).with_context(|| {
        format!(
            "failed to create {}",
            paths.report_root.join("reports").display()
        )
    })?;
    for group in &manifest.groups {
        let report_path = PathBuf::from(&group.report_path);
        if report_path.exists() && matches!(group.synthesis_status, StageStatus::Complete) {
            continue;
        }
        let mut body = String::new();
        body.push_str(&format!("# Audit Report: {}\n\n", group.name));
        body.push_str("## Scope\n\n");
        body.push_str("This report is assembled from first-pass one-file analyses. The synthesis pass may revise it based on cross-file relationships.\n\n");
        body.push_str("The authoritative first-pass inputs are the artifact paths listed under each file below. Ignore unreferenced artifact directories; interrupted or upgraded runs may leave stale artifacts in `audit/everything/*/files`.\n\n");
        body.push_str("## Debt Register\n\n");
        body.push_str("Synthesis must classify debt candidates with `safe_delete`, `deprecated_remove`, `consolidate`, `simplify`, `deepen_module`, or `leave_with_reason`. Each item needs path(s), action, proof found, proof still missing, behavior-preservation needs, and risk. If no candidates exist, write `No actionable debt candidates found.`\n\n");
        for file_path in &group.files {
            if let Some(file) = manifest.files.iter().find(|file| &file.path == file_path) {
                body.push_str(&format!("## `{}`\n\n", file.path));
                let analysis = Path::new(&file.artifact_dir).join("analysis.md");
                body.push_str(&format!("First-pass artifact: `{}`\n\n", file.artifact_dir));
                if analysis.exists() {
                    body.push_str(
                        &fs::read_to_string(&analysis)
                            .with_context(|| format!("failed to read {}", analysis.display()))?,
                    );
                    body.push_str("\n\n");
                } else {
                    body.push_str("_First-pass analysis missing._\n\n");
                }
            }
        }
        crate::util::atomic_write(&report_path, body.as_bytes())
            .with_context(|| format!("failed to write {}", report_path.display()))?;
    }
    Ok(())
}

pub(crate) fn prompt_file_body(path: &Path) -> Result<String> {
    let bytes = fs::read(path).with_context(|| format!("failed to read {}", path.display()))?;
    let byte_len = bytes.len();
    match String::from_utf8(bytes) {
        Ok(text) if byte_len > MAX_FILE_PROMPT_BYTES => {
            let line_count = text.lines().count();
            Ok(format!(
                "[large UTF-8 file omitted from inline prompt because it is {byte_len} bytes and {line_count} lines. Mandatory full-file review: inspect `{}` directly inside the worktree before writing artifacts. Read the entire file in ordered chunks no larger than 250 lines, from line 1 through line {line_count}, using `sed -n '<start>,<end>p'`, `nl -ba`, or an equivalent command. Do not sample. Do not rely on metadata only. In `analysis.md`, include a Coverage note that states this was a large-file chunked review and names the line count. In `analysis.json`, set `coverage` to a concise statement confirming full-file chunked inspection. If you cannot inspect every line, fail instead of writing artifacts.]",
                path.display()
            ))
        }
        Ok(text) => Ok(text),
        Err(err) => Ok(format!(
            "[binary or non-UTF8 file omitted from prompt: {} valid bytes before error]",
            err.utf8_error().valid_up_to()
        )),
    }
}

pub(crate) fn artifact_complete(artifact_dir: &Path) -> bool {
    artifact_dir.join("analysis.md").exists()
        && artifact_dir.join("analysis.json").exists()
        && !artifact_has_legacy_large_file_prompt(artifact_dir)
}

fn artifact_has_legacy_large_file_prompt(artifact_dir: &Path) -> bool {
    fs::read_to_string(artifact_dir.join("first-pass-prompt.md"))
        .is_ok_and(|prompt| prompt.contains(LEGACY_LARGE_FILE_OMISSION_MARKER))
}

fn file_artifact_dir(report_root: &Path, path: &str, content_hash: &str) -> PathBuf {
    report_root
        .join("files")
        .join(file_artifact_slug(path, content_hash))
}

pub(crate) fn file_artifact_slug(path: &str, content_hash: &str) -> String {
    let path_hash = sha256_hex(path.as_bytes());
    format!("{}-{}", short_hash(&path_hash), short_hash(content_hash))
}

fn legacy_file_artifact_dir(report_root: &Path, content_hash: &str) -> PathBuf {
    report_root.join("files").join(short_hash(content_hash))
}

fn migrate_legacy_file_artifact_if_matching(
    legacy_artifact_dir: &Path,
    artifact_dir: &Path,
    path: &str,
) -> Result<()> {
    if artifact_complete(artifact_dir)
        || !artifact_complete(legacy_artifact_dir)
        || !artifact_matches_path(legacy_artifact_dir, path)
    {
        return Ok(());
    }
    fs::create_dir_all(artifact_dir)
        .with_context(|| format!("failed to create {}", artifact_dir.display()))?;
    for file_name in ["analysis.md", "analysis.json"] {
        fs::copy(
            legacy_artifact_dir.join(file_name),
            artifact_dir.join(file_name),
        )
        .with_context(|| {
            format!(
                "failed to migrate {} from {} to {}",
                file_name,
                legacy_artifact_dir.display(),
                artifact_dir.display()
            )
        })?;
    }
    Ok(())
}

fn artifact_matches_path(artifact_dir: &Path, path: &str) -> bool {
    let json = fs::read_to_string(artifact_dir.join("analysis.json")).unwrap_or_default();
    if let Ok(value) = serde_json::from_str::<serde_json::Value>(&json) {
        if ["path", "file"]
            .iter()
            .filter_map(|key| value.get(*key).and_then(|value| value.as_str()))
            .any(|value| value == path)
        {
            return true;
        }
    }
    fs::read_to_string(artifact_dir.join("analysis.md"))
        .map(|markdown| {
            markdown
                .lines()
                .next()
                .is_some_and(|line| line.trim() == format!("# {path}"))
        })
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::{
        artifact_complete, build_initial_group_reports, excluded_path, fallback_group,
        file_artifact_slug, prompt_file_body,
    };
    use crate::audit_everything::manifest::{FileState, GroupState, StageStatus};
    use crate::audit_everything::run_paths::{RunPaths, PAUSE_REQUEST_FILE};
    use crate::audit_everything::sha256_hex;
    use crate::audit_everything::tests::manifest_with_groups;
    use std::fs;

    #[test]
    fn fallback_group_classifies_common_repo_surfaces() {
        assert_eq!(
            fallback_group("crates/bitino-house/src/lib.rs"),
            "crates/bitino-house"
        );
        assert_eq!(fallback_group("src/main.rs"), "src");
        assert_eq!(fallback_group("tests/parallel_status.rs"), "tests");
        assert_eq!(fallback_group("docs/ops/runbook.md"), "docs");
        assert_eq!(fallback_group("Cargo.toml"), "Cargo.toml");
    }

    #[test]
    fn excluded_path_skips_generated_and_runtime_state() {
        assert!(excluded_path(".auto/audit/log"));
        assert!(excluded_path(".claude/worktrees/agent-a123"));
        assert!(excluded_path(".claude/worktrees/agent-a123/README.md"));
        assert!(excluded_path(
            "audit/everything/20260424-115535/reports/src.md"
        ));
        assert!(excluded_path("audit/old-run/FINAL-REVIEW.md"));
        assert!(excluded_path("docs/ops/operator-evidence/canary.md"));
        assert!(excluded_path("web/client/dist/bitino-client.js"));
        assert!(excluded_path("web/play/dist/rplay.js"));
        assert!(excluded_path(".github/ISSUE_TEMPLATE/bug.md"));
        assert!(excluded_path("Cargo.lock"));
        assert!(excluded_path("web/play/dist/rplay.js.map"));
        assert!(excluded_path("docs/ops/operator-evidence/smoke.png"));
        assert!(excluded_path(
            "audit/everything/20260424-115535/files/hash/analysis.md"
        ));
        assert!(excluded_path("target/debug/app"));
        assert!(excluded_path("gen-20260424/spec.md"));
        assert!(!excluded_path("crates/bitino-house/src/lib.rs"));
    }

    #[test]
    fn file_artifact_slug_is_per_file_even_for_identical_content() {
        let content_hash = sha256_hex(b"same generated content");
        assert_ne!(
            file_artifact_slug("crates/a/generated.d.ts", &content_hash),
            file_artifact_slug("crates/b/generated.d.ts", &content_hash)
        );
    }

    #[test]
    fn large_utf8_file_prompt_requires_full_chunked_review() {
        let dir =
            std::env::temp_dir().join(format!("auto-audit-large-file-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("failed to create temp dir");
        let path = dir.join("large.rs");
        let mut body = String::new();
        for index in 0..20_000 {
            body.push_str(&format!("fn line_{index}() {{}}\n"));
        }
        fs::write(&path, body).expect("failed to write large file");

        let prompt_body = prompt_file_body(&path).expect("failed to build prompt file body");
        assert!(prompt_body.contains("large UTF-8 file omitted from inline prompt"));
        assert!(prompt_body.contains("Mandatory full-file review"));
        assert!(prompt_body.contains("Read the entire file in ordered chunks"));
        assert!(!prompt_body.contains("metadata and path only"));

        fs::remove_dir_all(&dir).expect("failed to remove temp dir");
    }

    #[test]
    fn legacy_large_file_prompt_invalidates_artifact_completion() {
        let dir =
            std::env::temp_dir().join(format!("auto-audit-legacy-artifact-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("failed to create temp dir");
        fs::write(dir.join("analysis.md"), "# src/lib.rs\n").expect("failed to write analysis.md");
        fs::write(dir.join("analysis.json"), "{}\n").expect("failed to write analysis.json");
        fs::write(
            dir.join("first-pass-prompt.md"),
            "[file omitted from prompt because it is 300000 bytes; inspect metadata and path only]",
        )
        .expect("failed to write first-pass prompt");

        assert!(!artifact_complete(&dir));

        fs::remove_dir_all(&dir).expect("failed to remove temp dir");
    }

    #[test]
    fn pending_group_report_rebuilds_with_authoritative_artifact_refs() {
        let dir = std::env::temp_dir().join(format!(
            "auto-audit-group-report-rebuild-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&dir);
        let report_root = dir.join("audit/everything/test-run");
        let artifact_dir = report_root.join("files/path-hash-content-hash");
        fs::create_dir_all(&artifact_dir).expect("failed to create artifact dir");
        fs::write(
            artifact_dir.join("analysis.md"),
            "# src/lib.rs\n\nA focused first-pass analysis.\n",
        )
        .expect("failed to write analysis");

        let report_path = report_root.join("reports/src.md");
        fs::create_dir_all(report_path.parent().unwrap()).expect("failed to create reports dir");
        fs::write(&report_path, "stale partial synthesis\n").expect("failed to write stale report");

        let mut manifest = manifest_with_groups(vec![GroupState {
            name: "src".to_string(),
            slug: "src".to_string(),
            files: vec!["src/lib.rs".to_string()],
            report_path: report_path.display().to_string(),
            synthesis_status: StageStatus::Pending,
            remediation_status: StageStatus::Pending,
        }]);
        manifest.files = vec![FileState {
            path: "src/lib.rs".to_string(),
            group: "src".to_string(),
            content_hash: "content-hash".to_string(),
            artifact_dir: artifact_dir.display().to_string(),
            status: StageStatus::Complete,
        }];

        let paths = RunPaths {
            host_root: dir.clone(),
            manifest_path: dir.join("manifest.json"),
            latest_path: dir.join("latest"),
            worktree_root: dir.clone(),
            report_root,
            pause_path: dir.join(PAUSE_REQUEST_FILE),
            in_place: false,
        };

        build_initial_group_reports(&paths, &manifest).expect("failed to build group reports");
        let report = fs::read_to_string(&report_path).expect("failed to read report");

        assert!(!report.contains("stale partial synthesis"));
        assert!(report.contains("First-pass artifact:"));
        assert!(report.contains("path-hash-content-hash"));
        assert!(report.contains("Ignore unreferenced artifact directories"));
        assert!(report.contains("## Debt Register"));
        assert!(report.contains("safe_delete"));

        fs::remove_dir_all(&dir).expect("failed to remove temp dir");
    }
}
