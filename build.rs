use std::path::{Path, PathBuf};
use std::process::Command;

fn main() {
    println!("cargo:rerun-if-env-changed=PROFILE");
    emit_git_rerun_markers();

    let git_sha = git_output(&["rev-parse", "--short", "HEAD"]).unwrap_or_else(|| "unknown".into());
    let git_dirty = git_dirty_flag();
    let profile = std::env::var("PROFILE").unwrap_or_else(|_| "unknown".into());

    println!("cargo:rustc-env=AUTODEV_GIT_SHA={git_sha}");
    println!("cargo:rustc-env=AUTODEV_GIT_DIRTY={git_dirty}");
    println!("cargo:rustc-env=AUTODEV_BUILD_PROFILE={profile}");
}

fn emit_git_rerun_markers() {
    let Some(git_dir) = git_output(&["rev-parse", "--git-dir"]).map(PathBuf::from) else {
        return;
    };

    emit_rerun_if_changed(git_dir.join("HEAD"));
    emit_rerun_if_changed(git_dir.join("packed-refs"));

    if let Some(head_ref) = git_output(&["symbolic-ref", "HEAD"]) {
        emit_rerun_if_changed(git_dir.join(head_ref));
    }
    if let Some(index_path) = git_output(&["rev-parse", "--git-path", "index"]) {
        emit_rerun_if_changed(PathBuf::from(index_path));
    }
}

fn emit_rerun_if_changed(path: impl AsRef<Path>) {
    println!("cargo:rerun-if-changed={}", path.as_ref().display());
}

fn git_output(args: &[&str]) -> Option<String> {
    let output = Command::new("git").args(args).output().ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8(output.stdout).ok()?;
    Some(text.trim().to_string())
}

fn git_dirty_flag() -> &'static str {
    let output = Command::new("git")
        .args(["status", "--porcelain"])
        .output()
        .ok();
    let Some(output) = output else {
        return "unknown";
    };
    if !output.status.success() {
        return "unknown";
    }
    if output.stdout.is_empty() {
        "clean"
    } else {
        "dirty"
    }
}
