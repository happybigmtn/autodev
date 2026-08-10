use std::fs;
use std::path::Path;

use anyhow::{Context, Result};

use crate::util::atomic_write;

pub(crate) const VERIFICATION_WRAPPER_RELATIVE: &str = "scripts/run-task-verification.sh";
const VERIFICATION_WRAPPER_TEMPLATE: &str = include_str!("../scripts/run-task-verification.sh");

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum VerificationWrapperScaffold {
    Present,
    Installed,
    FixedMode,
}

pub(crate) fn scaffold_verification_wrapper(
    repo_root: &Path,
) -> Result<VerificationWrapperScaffold> {
    let path = repo_root.join(VERIFICATION_WRAPPER_RELATIVE);
    if path.exists() {
        ensure_executable(&path)?;
        return Ok(VerificationWrapperScaffold::Present);
    }
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }
    atomic_write(&path, VERIFICATION_WRAPPER_TEMPLATE.as_bytes())
        .with_context(|| format!("failed to write {}", path.display()))?;
    ensure_executable(&path)?;
    Ok(VerificationWrapperScaffold::Installed)
}

pub(crate) fn ensure_verification_wrapper_executable(
    repo_root: &Path,
) -> Result<VerificationWrapperScaffold> {
    let path = repo_root.join(VERIFICATION_WRAPPER_RELATIVE);
    if !path.exists() {
        return scaffold_verification_wrapper(repo_root);
    }
    if ensure_executable(&path)? {
        Ok(VerificationWrapperScaffold::FixedMode)
    } else {
        Ok(VerificationWrapperScaffold::Present)
    }
}

fn ensure_executable(path: &Path) -> Result<bool> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        let metadata =
            fs::metadata(path).with_context(|| format!("failed to stat {}", path.display()))?;
        let mut permissions = metadata.permissions();
        let mode = permissions.mode();
        if mode & 0o111 == 0o111 {
            return Ok(false);
        }
        permissions.set_mode(mode | 0o755);
        fs::set_permissions(path, permissions)
            .with_context(|| format!("failed to chmod {}", path.display()))?;
        Ok(true)
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        Ok(false)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_dir(name: &str) -> std::path::PathBuf {
        let path = std::env::temp_dir().join(format!(
            "autodev-wrapper-{name}-{}",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("time went backwards")
                .as_nanos()
        ));
        fs::create_dir_all(&path).expect("create temp dir");
        path
    }

    #[test]
    fn scaffold_installs_template_and_is_idempotent() {
        let repo = temp_dir("install");
        let status = scaffold_verification_wrapper(&repo).expect("scaffold should succeed");
        assert_eq!(status, VerificationWrapperScaffold::Installed);
        let path = repo.join(VERIFICATION_WRAPPER_RELATIVE);
        assert!(path.is_file());
        assert!(fs::read_to_string(&path)
            .expect("read wrapper")
            .contains("verification-receipt: recorded"));

        let status = scaffold_verification_wrapper(&repo).expect("second scaffold should succeed");
        assert_eq!(status, VerificationWrapperScaffold::Present);

        fs::remove_dir_all(&repo).ok();
    }

    #[test]
    fn rerunning_command_preserves_explicit_supersession_history() {
        let repo = temp_dir("preserve-supersedes");
        Command::new("git")
            .args(["init", "-q"])
            .current_dir(&repo)
            .status()
            .expect("initialize test repository");
        Command::new("git")
            .args(["config", "user.email", "autodev-test@example.invalid"])
            .current_dir(&repo)
            .status()
            .expect("configure test email");
        Command::new("git")
            .args(["config", "user.name", "AutoDev Test"])
            .current_dir(&repo)
            .status()
            .expect("configure test name");
        fs::write(repo.join("tracked.txt"), "receipt fixture\n").expect("write fixture");
        Command::new("git")
            .args(["add", "tracked.txt"])
            .current_dir(&repo)
            .status()
            .expect("stage fixture");
        Command::new("git")
            .args(["commit", "-qm", "test fixture"])
            .current_dir(&repo)
            .status()
            .expect("commit fixture");

        scaffold_verification_wrapper(&repo).expect("install wrapper");
        let wrapper = repo.join(VERIFICATION_WRAPPER_RELATIVE);
        let first = Command::new(&wrapper)
            .args(["TASK-PRESERVE", "--", "bash", "-c", "true"])
            .env("AUTO_SUPERSEDES", "legacy failing command")
            .current_dir(&repo)
            .status()
            .expect("record superseding verification");
        assert!(first.success());

        let second = Command::new(&wrapper)
            .args(["TASK-PRESERVE", "--", "bash", "-c", "true"])
            .env_remove("AUTO_SUPERSEDES")
            .current_dir(&repo)
            .status()
            .expect("rerun verification");
        assert!(second.success());

        let receipt: serde_json::Value = serde_json::from_slice(
            &fs::read(repo.join(".auto/symphony/verification-receipts/TASK-PRESERVE.json"))
                .expect("read receipt"),
        )
        .expect("parse receipt");
        let command = receipt["commands"]
            .as_array()
            .and_then(|commands| {
                commands
                    .iter()
                    .find(|entry| entry["command"] == "bash -c true")
            })
            .expect("recorded command");
        assert_eq!(
            command["supersedes"],
            serde_json::json!(["legacy failing command"])
        );

        fs::remove_dir_all(&repo).ok();
    }
}
