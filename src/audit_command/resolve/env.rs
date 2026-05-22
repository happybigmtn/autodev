//! Lane validation environment: the cargo guard wrapper, lane-scoped Cargo env
//! vars, and `cargo` / `auto` executable resolution.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{bail, Context, Result};

use crate::util::atomic_write;

pub(crate) fn prepare_finding_resolution_lane_env(
    repo_root: &Path,
    lane_target_dir: &Path,
    validation_threads: usize,
) -> Result<Vec<(String, String)>> {
    fs::create_dir_all(lane_target_dir)
        .with_context(|| format!("failed to create {}", lane_target_dir.display()))?;
    let bin_dir = lane_target_dir.join("autodev-bin");
    fs::create_dir_all(&bin_dir)
        .with_context(|| format!("failed to create {}", bin_dir.display()))?;
    let real_cargo = resolve_real_cargo()?;
    let wrapper = bin_dir.join("cargo");
    atomic_write(&wrapper, cargo_guard_wrapper_script().as_bytes())
        .with_context(|| format!("failed to write {}", wrapper.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut permissions = fs::metadata(&wrapper)
            .with_context(|| format!("failed to stat {}", wrapper.display()))?
            .permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&wrapper, permissions)
            .with_context(|| format!("failed to chmod {}", wrapper.display()))?;
    }
    let current_path = std::env::var("PATH").unwrap_or_else(|_| "/usr/bin:/bin".to_string());
    let path = format!("{}:{current_path}", bin_dir.display());
    Ok(vec![
        (
            "CARGO_TARGET_DIR".to_string(),
            lane_target_dir.display().to_string(),
        ),
        (
            "CARGO_BUILD_JOBS".to_string(),
            validation_threads.max(1).to_string(),
        ),
        ("AUTODEV_REAL_CARGO".to_string(), real_cargo),
        ("PATH".to_string(), path),
        (
            "AUTO_AUDIT_RESOLVE_VALIDATION_THREADS".to_string(),
            validation_threads.max(1).to_string(),
        ),
        (
            "AUTO_AUDIT_REPO_ROOT".to_string(),
            repo_root.display().to_string(),
        ),
    ])
}

fn resolve_real_cargo() -> Result<String> {
    if let Ok(path) = std::env::var("AUTODEV_REAL_CARGO") {
        if !path.trim().is_empty() {
            return Ok(path);
        }
    }
    let output = Command::new("sh")
        .arg("-lc")
        .arg("command -v cargo")
        .output()
        .context("failed to resolve cargo executable")?;
    if !output.status.success() {
        bail!(
            "failed to resolve cargo executable: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    let path = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if path.is_empty() {
        bail!("failed to resolve cargo executable: command -v cargo returned empty output");
    }
    Ok(path)
}

pub(crate) fn resolve_auto_executable() -> Result<PathBuf> {
    if let Ok(path) = std::env::var("AUTODEV_AUTO_BIN") {
        let path = PathBuf::from(path);
        if path.exists() {
            return Ok(path);
        }
    }
    if let Ok(current) = std::env::current_exe() {
        if current.exists() {
            return Ok(current);
        }
        let current_text = current.to_string_lossy();
        if let Some(stripped) = current_text.strip_suffix(" (deleted)") {
            let stripped = PathBuf::from(stripped);
            if stripped.exists() {
                return Ok(stripped);
            }
        }
    }
    let output = Command::new("sh")
        .arg("-lc")
        .arg("command -v auto")
        .output()
        .context("failed to resolve auto executable from PATH")?;
    if output.status.success() {
        let path = PathBuf::from(String::from_utf8_lossy(&output.stdout).trim().to_string());
        if path.exists() {
            return Ok(path);
        }
    }
    bail!(
        "failed to resolve auto executable; set AUTODEV_AUTO_BIN to the installed auto binary path"
    )
}

fn cargo_guard_wrapper_script() -> &'static str {
    r#"#!/usr/bin/env bash
set -euo pipefail
real="${AUTODEV_REAL_CARGO:?AUTODEV_REAL_CARGO is required}"

if [[ "${1:-}" == "test" ]]; then
  filters=0
  skip_next=0
  after_dashdash=0
  for arg in "${@:2}"; do
    if [[ "$after_dashdash" == "1" ]]; then
      continue
    fi
    if [[ "$skip_next" == "1" ]]; then
      skip_next=0
      continue
    fi
    case "$arg" in
      --)
        after_dashdash=1
        continue
        ;;
      -p|--package|--manifest-path|--target|--target-dir|--bin|--test|--bench|--example|--features|--color|--message-format|--jobs|--profile)
        skip_next=1
        continue
        ;;
      --package=*|--manifest-path=*|--target=*|--target-dir=*|--bin=*|--test=*|--bench=*|--example=*|--features=*|--color=*|--message-format=*|--jobs=*|--profile=*)
        continue
        ;;
      --lib|--bins|--tests|--benches|--examples|--all-targets|--all-features|--no-default-features|--workspace|--all|--locked|--offline|--frozen|--release|--no-fail-fast|--doc|--quiet|--verbose|-q|-v)
        continue
        ;;
      -*)
        continue
        ;;
      *)
        filters=$((filters + 1))
        ;;
    esac
  done
  if (( filters > 1 )); then
    echo "AUTO_AUDIT_CARGO_FILTER_ERROR: cargo test accepts only one test filter. Split exact tests into separate commands or use one common module-level filter." >&2
    exit 64
  fi
fi

exec "$real" "$@"
"#
}
