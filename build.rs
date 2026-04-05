use std::process::Command;

fn main() {
    println!("cargo:rerun-if-changed=.git/HEAD");
    println!("cargo:rerun-if-env-changed=PROFILE");

    let git_sha = git_output(&["rev-parse", "--short", "HEAD"]).unwrap_or_else(|| "unknown".into());
    let git_dirty = git_dirty_flag();
    let profile = std::env::var("PROFILE").unwrap_or_else(|_| "unknown".into());

    println!("cargo:rustc-env=AUTODEV_GIT_SHA={git_sha}");
    println!("cargo:rustc-env=AUTODEV_GIT_DIRTY={git_dirty}");
    println!("cargo:rustc-env=AUTODEV_BUILD_PROFILE={profile}");
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
