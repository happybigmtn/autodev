//! Conservative Cargo-workspace input fingerprinting for verification reuse.
//!
//! A full source fingerprint correctly invalidates on every repository change,
//! but that makes Python operator evidence and curriculum edits rerun an entire
//! Rust workspace. This narrower fingerprint still hashes every current file
//! under each workspace member and local path dependency (including arbitrary
//! fixtures/assets), together with root Cargo and toolchain configuration.
//! Metadata or inventory ambiguity returns an error so callers run Cargo.

use anyhow::{bail, Context, Result};
use regex::Regex;
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::Command;
#[cfg(test)]
use std::time::SystemTime;

const MAX_WORKSPACE_INPUT_ENTRIES: usize = 200_000;
const MAX_WORKSPACE_INPUT_BYTES: u64 = 1_073_741_824;

#[derive(Deserialize)]
struct CargoMetadataInput {
    packages: Vec<CargoPackageInput>,
}

#[derive(Deserialize)]
struct CargoPackageInput {
    manifest_path: String,
    #[serde(default)]
    dependencies: Vec<CargoDependencyInput>,
    #[serde(default)]
    targets: Vec<CargoTargetInput>,
}

#[derive(Deserialize)]
struct CargoDependencyInput {
    path: Option<String>,
}

#[derive(Deserialize)]
struct CargoTargetInput {
    src_path: String,
}

pub(crate) fn current_workspace_probe_input_fingerprint(repo_root: &Path) -> Result<String> {
    let repo_root = fs::canonicalize(repo_root)
        .with_context(|| format!("failed to canonicalize {}", repo_root.display()))?;
    let output = Command::new("cargo")
        .args(["metadata", "--no-deps", "--format-version", "1"])
        .current_dir(&repo_root)
        .output()
        .context("failed to run cargo metadata for workspace input fingerprint")?;
    if !output.status.success() {
        bail!(
            "cargo metadata failed while collecting workspace inputs: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    let metadata: CargoMetadataInput = serde_json::from_slice(&output.stdout)
        .context("cargo metadata returned malformed workspace input JSON")?;

    let mut recursive_roots = BTreeSet::new();
    let mut exact_paths = BTreeSet::new();
    for package in metadata.packages {
        let manifest = canonical_repo_input(&repo_root, Path::new(&package.manifest_path))?;
        exact_paths.insert(manifest.clone());
        recursive_roots.insert(
            manifest
                .parent()
                .context("Cargo package manifest had no parent")?
                .to_path_buf(),
        );
        for dependency in package.dependencies {
            if let Some(path) = dependency.path {
                recursive_roots.insert(canonical_repo_input(&repo_root, Path::new(&path))?);
            }
        }
        for target in package.targets {
            exact_paths.insert(canonical_repo_input(
                &repo_root,
                Path::new(&target.src_path),
            )?);
        }
    }

    for path in [
        "Cargo.toml",
        "Cargo.lock",
        "rust-toolchain",
        "rust-toolchain.toml",
    ] {
        exact_paths.insert(repo_root.join(path));
    }
    recursive_roots.insert(repo_root.join(".cargo"));

    let mut records = workspace_git_inventory(&repo_root, &["ls-files", "--stage", "-z"])?;
    let untracked = workspace_git_inventory(
        &repo_root,
        &["ls-files", "--others", "--exclude-standard", "-z"],
    )?;
    for path in untracked {
        records.push(format!("untracked\t{path}"));
    }
    let ignored = workspace_git_inventory(
        &repo_root,
        &[
            "ls-files",
            "--others",
            "--ignored",
            "--exclude-standard",
            "-z",
            "--",
            ".",
            ":(exclude)**/target/**",
            ":(exclude)**/node_modules/**",
            ":(exclude)**/.auto/**",
            ":(exclude)**/bug/**",
            ":(exclude)**/nemesis/**",
            ":(exclude)**/gen-*/**",
        ],
    )?;
    for path in ignored {
        records.push(format!("ignored\t{path}"));
    }
    records.sort();
    discover_literal_external_inputs(&repo_root, &records, &mut recursive_roots, &mut exact_paths)?;

    let mut selected = 0_usize;
    let mut bytes = 0_u64;
    let mut digest = Sha256::new();
    digest.update(b"autodev-cargo-workspace-inputs-v1\0");
    for record in records {
        let (metadata, path) = record
            .split_once('\t')
            .context("git workspace input inventory entry lacked a path")?;
        let relative = Path::new(path);
        if workspace_generated_path(relative) {
            continue;
        }
        let absolute = repo_root.join(relative);
        if !exact_paths.contains(&absolute)
            && !recursive_roots
                .iter()
                .any(|root| absolute.starts_with(root))
        {
            continue;
        }
        if metadata.split_whitespace().next() == Some("160000") {
            bail!("workspace input `{path}` is a gitlink; refusing partial fingerprint reuse");
        }
        selected += 1;
        if selected > MAX_WORKSPACE_INPUT_ENTRIES {
            bail!("Cargo workspace input inventory exceeded {MAX_WORKSPACE_INPUT_ENTRIES} entries");
        }
        hash_field(&mut digest, b"path", path.as_bytes());
        hash_field(&mut digest, b"git", metadata.as_bytes());
        hash_workspace_path(&absolute, path, &mut bytes, &mut digest)?;
    }
    Ok(format!("{:x}", digest.finalize()))
}

/// Cargo/Rust dep-info covers compile-time `include_*` inputs, but a test may
/// also open a repository script or fixture at runtime. Keep the reusable
/// fingerprint conservative by discovering literal references from member Rust
/// sources to any top-level repository directory (for example
/// `../../scripts/...` or `../../../tools/...`). Dynamic suffixes widen to the
/// existing directory prefix. A source change that introduces a new reference
/// changes the member fingerprint and therefore forces a full probe first.
fn discover_literal_external_inputs(
    repo_root: &Path,
    records: &[String],
    recursive_roots: &mut BTreeSet<PathBuf>,
    exact_paths: &mut BTreeSet<PathBuf>,
) -> Result<()> {
    let mut top_level_dirs = Vec::new();
    for entry in fs::read_dir(repo_root).context("failed to list repository input roots")? {
        let entry = entry.context("failed to inspect repository input root")?;
        if entry.file_type().is_ok_and(|kind| kind.is_dir()) {
            let name = entry.file_name();
            if !matches!(
                name.to_str(),
                Some(".git" | ".auto" | "target" | "node_modules")
            ) {
                top_level_dirs.push(name.to_string_lossy().into_owned());
            }
        }
    }
    top_level_dirs.sort();

    let quoted = Regex::new(r#""([^"\r\n]+)""#).expect("static quoted-string regex");
    let member_roots = recursive_roots.clone();
    for record in records {
        let Some((_, path)) = record.split_once('\t') else {
            continue;
        };
        if !path.ends_with(".rs") {
            continue;
        }
        let absolute = repo_root.join(path);
        if !member_roots.iter().any(|root| absolute.starts_with(root)) {
            continue;
        }
        let Ok(source) = fs::read_to_string(&absolute) else {
            continue;
        };
        for capture in quoted.captures_iter(&source) {
            let literal = capture.get(1).expect("capture exists").as_str();
            for top in &top_level_dirs {
                let needle = format!("{top}/");
                let Some(offset) = literal.find(&needle) else {
                    continue;
                };
                let suffix = literal[offset..]
                    .split(|ch: char| {
                        ch.is_whitespace() || matches!(ch, '"' | '\'' | ')' | ']' | ';' | ',')
                    })
                    .next()
                    .unwrap_or("");
                let stable_prefix = suffix.split(['{', '$']).next().unwrap_or("");
                if stable_prefix.is_empty() {
                    continue;
                }
                let candidate = repo_root.join(stable_prefix);
                add_existing_external_input(repo_root, &candidate, recursive_roots, exact_paths)?;
            }
        }
    }
    Ok(())
}

fn add_existing_external_input(
    repo_root: &Path,
    candidate: &Path,
    recursive_roots: &mut BTreeSet<PathBuf>,
    exact_paths: &mut BTreeSet<PathBuf>,
) -> Result<()> {
    let candidate = if candidate.exists() {
        candidate.to_path_buf()
    } else {
        candidate
            .parent()
            .filter(|parent| parent.exists())
            .unwrap_or(candidate)
            .to_path_buf()
    };
    let canonical = fs::canonicalize(&candidate).with_context(|| {
        format!(
            "failed to canonicalize external Cargo runtime input {}",
            candidate.display()
        )
    })?;
    if !canonical.starts_with(repo_root) {
        bail!(
            "external Cargo runtime input {} escaped repository {}",
            canonical.display(),
            repo_root.display()
        );
    }
    if canonical.is_dir() {
        recursive_roots.insert(canonical);
    } else {
        exact_paths.insert(canonical);
    }
    Ok(())
}

fn canonical_repo_input(repo_root: &Path, path: &Path) -> Result<PathBuf> {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        repo_root.join(path)
    };
    let canonical = fs::canonicalize(&absolute)
        .with_context(|| format!("failed to canonicalize Cargo input {}", absolute.display()))?;
    if !canonical.starts_with(repo_root) {
        bail!(
            "Cargo input {} is outside repository {}; refusing partial fingerprint reuse",
            canonical.display(),
            repo_root.display()
        );
    }
    Ok(canonical)
}

fn workspace_git_inventory(repo_root: &Path, args: &[&str]) -> Result<Vec<String>> {
    let output = Command::new("git")
        .args(args)
        .current_dir(repo_root)
        .output()
        .context("failed to collect Cargo workspace git inventory")?;
    if !output.status.success() {
        bail!(
            "git workspace input inventory failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    output
        .stdout
        .split(|byte| *byte == 0)
        .filter(|record| !record.is_empty())
        .map(|record| {
            std::str::from_utf8(record)
                .context("Cargo workspace git inventory was not UTF-8")
                .map(str::to_string)
        })
        .collect()
}

fn workspace_generated_path(path: &Path) -> bool {
    path.components().any(|component| {
        component.as_os_str().to_str().is_some_and(|name| {
            matches!(
                name,
                ".git" | ".auto" | "bug" | "nemesis" | "target" | "node_modules"
            ) || name.starts_with("gen-")
        })
    })
}

fn hash_workspace_path(
    path: &Path,
    label: &str,
    total_bytes: &mut u64,
    digest: &mut Sha256,
) -> Result<()> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            hash_field(digest, b"worktree", b"missing");
            return Ok(());
        }
        Err(err) => return Err(err).with_context(|| format!("failed to stat `{label}`")),
    };
    hash_field(
        digest,
        b"filesystem-mode",
        &workspace_file_mode(&metadata).to_be_bytes(),
    );
    if metadata.file_type().is_symlink() {
        let target = fs::read_link(path)
            .with_context(|| format!("failed to read workspace input symlink `{label}`"))?;
        hash_field(digest, b"symlink", target.as_os_str().as_encoded_bytes());
        return Ok(());
    }
    if !metadata.is_file() {
        bail!("unsupported Cargo workspace input type at `{label}`");
    }
    *total_bytes = total_bytes.saturating_add(metadata.len());
    if *total_bytes > MAX_WORKSPACE_INPUT_BYTES {
        bail!("Cargo workspace inputs exceeded {MAX_WORKSPACE_INPUT_BYTES} bytes");
    }
    let mut file = fs::File::open(path)
        .with_context(|| format!("failed to open Cargo workspace input `{label}`"))?;
    let mut content = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = file
            .read(&mut buffer)
            .with_context(|| format!("failed to read Cargo workspace input `{label}`"))?;
        if read == 0 {
            break;
        }
        content.update(&buffer[..read]);
    }
    hash_field(digest, b"content", &content.finalize());
    Ok(())
}

#[cfg(unix)]
fn workspace_file_mode(metadata: &fs::Metadata) -> u32 {
    use std::os::unix::fs::PermissionsExt;
    metadata.permissions().mode()
}

#[cfg(not(unix))]
fn workspace_file_mode(metadata: &fs::Metadata) -> u32 {
    u32::from(metadata.permissions().readonly())
}

fn hash_field(digest: &mut Sha256, label: &[u8], value: &[u8]) {
    digest.update((label.len() as u64).to_be_bytes());
    digest.update(label);
    digest.update((value.len() as u64).to_be_bytes());
    digest.update(value);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(name: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!(
            "auto-workspace-input-{name}-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(root.join("crates/app/src")).unwrap();
        fs::write(
            root.join("Cargo.toml"),
            "[workspace]\nresolver = \"2\"\nmembers = [\"crates/app\"]\n",
        )
        .unwrap();
        fs::write(
            root.join("crates/app/Cargo.toml"),
            "[package]\nname = \"app\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
        )
        .unwrap();
        fs::write(
            root.join("crates/app/src/lib.rs"),
            "const _: &str = include_str!(\"../../../docs/runtime.txt\");\npub fn value() -> u8 { 1 }\n",
        )
        .unwrap();
        fs::create_dir_all(root.join("docs")).unwrap();
        fs::write(root.join("docs/chapter.md"), "one\n").unwrap();
        fs::write(root.join("docs/runtime.txt"), "runtime one\n").unwrap();
        Command::new("git")
            .args(["init", "-q"])
            .current_dir(&root)
            .status()
            .unwrap();
        Command::new("git")
            .args(["add", "."])
            .current_dir(&root)
            .status()
            .unwrap();
        root
    }

    #[test]
    fn unrelated_docs_do_not_invalidate_workspace_inputs() {
        let root = fixture("docs");
        let before = current_workspace_probe_input_fingerprint(&root).unwrap();
        fs::write(root.join("docs/chapter.md"), "two\n").unwrap();
        let after = current_workspace_probe_input_fingerprint(&root).unwrap();
        assert_eq!(before, after);
        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn crate_sources_assets_manifests_and_untracked_files_invalidate() {
        let root = fixture("crate-inputs");
        let original = current_workspace_probe_input_fingerprint(&root).unwrap();
        fs::write(
            root.join("crates/app/src/lib.rs"),
            "pub fn value() -> u8 { 2 }\n",
        )
        .unwrap();
        let source = current_workspace_probe_input_fingerprint(&root).unwrap();
        assert_ne!(original, source);
        fs::write(root.join("crates/app/fixture.json"), "{}\n").unwrap();
        let untracked_asset = current_workspace_probe_input_fingerprint(&root).unwrap();
        assert_ne!(source, untracked_asset);
        fs::write(
            root.join("Cargo.toml"),
            "[workspace]\nresolver = \"2\"\nmembers = [\"crates/app\"]\n# changed\n",
        )
        .unwrap();
        let manifest = current_workspace_probe_input_fingerprint(&root).unwrap();
        assert_ne!(untracked_asset, manifest);
        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn ignored_runtime_files_invalidate_but_generated_build_trees_do_not() {
        let root = fixture("ignored-inputs");
        fs::write(root.join(".gitignore"), "private-fixture.txt\ntarget/\n").unwrap();
        fs::write(root.join("crates/app/private-fixture.txt"), "runtime one\n").unwrap();
        let original = current_workspace_probe_input_fingerprint(&root).unwrap();
        fs::write(root.join("crates/app/private-fixture.txt"), "runtime two\n").unwrap();
        let ignored_runtime = current_workspace_probe_input_fingerprint(&root).unwrap();
        assert_ne!(original, ignored_runtime);

        fs::create_dir_all(root.join("crates/app/target/debug")).unwrap();
        fs::write(root.join("crates/app/target/debug/generated.bin"), b"one").unwrap();
        let generated_one = current_workspace_probe_input_fingerprint(&root).unwrap();
        fs::write(root.join("crates/app/target/debug/generated.bin"), b"two").unwrap();
        let generated_two = current_workspace_probe_input_fingerprint(&root).unwrap();
        assert_eq!(generated_one, generated_two);
        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn literal_external_runtime_input_invalidates_but_its_sibling_does_not() {
        let root = fixture("external-runtime");
        let original = current_workspace_probe_input_fingerprint(&root).unwrap();
        fs::write(root.join("docs/chapter.md"), "two\n").unwrap();
        let unrelated = current_workspace_probe_input_fingerprint(&root).unwrap();
        assert_eq!(original, unrelated);
        fs::write(root.join("docs/runtime.txt"), "runtime two\n").unwrap();
        let runtime = current_workspace_probe_input_fingerprint(&root).unwrap();
        assert_ne!(unrelated, runtime);
        fs::remove_dir_all(root).ok();
    }
}
