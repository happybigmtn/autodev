mod fsutil;
mod git;
mod spawn;

use std::path::Path;

use chrono::Utc;

#[cfg(test)]
pub(crate) use fsutil::test_process_env_lock;
pub(crate) use fsutil::{
    atomic_write, atomic_write_0o600_if_unix, copy_tree, ensure_repo_layout, list_markdown_files,
    opencode_agent_dir, prune_pi_runtime_state, truncate_file_to_max_bytes, write_0o600_if_unix,
};
pub(crate) use git::{
    auto_checkpoint_if_needed, git_branch_exists, git_cherry_pick_empty_arg, git_repo_root,
    git_status_short_filtered, git_stdout, parse_origin_head_branch, push_branch_with_remote_sync,
    run_git, sync_branch_with_remote, KNOWN_PRIMARY_BRANCHES,
};
#[cfg(test)]
pub(crate) use spawn::output_retrying_etxtbsy;
pub(crate) use spawn::{spawn_retrying_etxtbsy, spawn_retrying_etxtbsy_tokio};

pub(crate) const CLI_LONG_VERSION: &str = concat!(
    env!("CARGO_PKG_VERSION"),
    "\ncommit: ",
    env!("AUTODEV_GIT_SHA"),
    "\ndirty: ",
    env!("AUTODEV_GIT_DIRTY"),
    "\nprofile: ",
    env!("AUTODEV_BUILD_PROFILE"),
);

pub(crate) fn repo_name(repo_root: &Path) -> String {
    repo_root
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("repo")
        .to_string()
}

pub(crate) fn timestamp_slug() -> String {
    Utc::now().format("%Y%m%d-%H%M%S").to_string()
}

/// Optional override base for auto's heavy, regenerable working directories.
///
/// When `$AUTO_RUN_ROOT` is set (non-empty), returns
/// `<AUTO_RUN_ROOT>/<repo-name>/<subdir>` so big `auto parallel` lane builds, corpus
/// staging, and gen/design snapshots land on a roomy volume instead of the repo's own
/// (possibly small) disk. Returns `None` — callers keep their in-repo default — when the
/// env var is unset or empty.
pub(crate) fn auto_run_root_override(repo_root: &Path, subdir: &str) -> Option<std::path::PathBuf> {
    let root = std::env::var("AUTO_RUN_ROOT").ok()?;
    let root = root.trim();
    if root.is_empty() {
        return None;
    }
    Some(Path::new(root).join(repo_name(repo_root)).join(subdir))
}

pub(crate) fn current_binary_path() -> String {
    std::env::current_exe()
        .ok()
        .map(|path| path.display().to_string())
        .unwrap_or_else(|| "unknown".to_string())
}

pub(crate) fn binary_provenance_line() -> String {
    format!(
        "{} @ {} ({}, {})",
        env!("CARGO_PKG_VERSION"),
        current_binary_path(),
        env!("AUTODEV_GIT_SHA"),
        env!("AUTODEV_GIT_DIRTY")
    )
}

pub(crate) fn clip_line_for_display(line: &str, max_chars: usize) -> String {
    line.chars().take(max_chars).collect()
}

#[cfg(test)]
mod tests {
    use super::{clip_line_for_display, CLI_LONG_VERSION};

    #[test]
    fn cli_long_version_exposes_build_provenance_metadata() {
        let lines: Vec<_> = CLI_LONG_VERSION.lines().collect();

        assert_eq!(lines.len(), 4);
        assert_eq!(lines[0], env!("CARGO_PKG_VERSION"));

        let commit = lines[1]
            .strip_prefix("commit: ")
            .expect("version should label the build commit");
        assert!(!commit.trim().is_empty());

        let dirty = lines[2]
            .strip_prefix("dirty: ")
            .expect("version should label the dirty-state flag");
        assert!(
            matches!(dirty, "clean" | "dirty" | "unknown"),
            "unexpected dirty-state flag: {dirty}"
        );

        let profile = lines[3]
            .strip_prefix("profile: ")
            .expect("version should label the cargo build profile");
        assert!(
            matches!(profile, "debug" | "release" | "unknown"),
            "unexpected build profile: {profile}"
        );
    }

    #[test]
    fn clips_on_char_boundaries() {
        let line = "╔══════════════════╗";
        let clipped = clip_line_for_display(line, 6);
        assert_eq!(clipped, "╔═════");
    }
}
