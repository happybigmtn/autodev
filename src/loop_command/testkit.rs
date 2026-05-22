//! Shared test fixtures for the `loop_command` submodules.

use std::fs;
use std::path::PathBuf;
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

pub(crate) fn unique_temp_dir(label: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("time went backwards")
        .as_nanos();
    std::env::temp_dir().join(format!("autodev-{label}-{nanos}"))
}

pub(crate) fn init_git_repo(path: &PathBuf) {
    fs::create_dir_all(path).expect("failed to create repo dir");
    let status = Command::new("git")
        .args(["init", "-q"])
        .arg(path)
        .status()
        .expect("failed to run git init");
    assert!(status.success(), "git init should succeed");
}
