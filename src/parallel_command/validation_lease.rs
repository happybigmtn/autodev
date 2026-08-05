//! Cross-process resource lease for canonical workspace validation.
//!
//! Lane Cargo shims take shared `flock(2)` leases. A canonical workspace probe
//! takes the exclusive side here, preventing load-sensitive overlap without
//! pausing agents that are reasoning, editing, or running non-Cargo checks.

use super::*;
use fs2::FileExt;

const VALIDATION_LEASE_FILE: &str = "validation-resource.lock";

pub(crate) fn validation_lease_path(run_root: &Path) -> PathBuf {
    run_root.join(VALIDATION_LEASE_FILE)
}

pub(crate) struct ExclusiveValidationLease {
    _file: fs::File,
    waited: Duration,
}

impl ExclusiveValidationLease {
    pub(crate) fn waited(&self) -> Duration {
        self.waited
    }
}

pub(crate) async fn acquire_exclusive_validation_lease(
    run_root: &Path,
) -> Result<ExclusiveValidationLease> {
    let path = validation_lease_path(run_root);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }
    let file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(&path)
        .with_context(|| format!("failed to open validation lease {}", path.display()))?;
    let started = Instant::now();
    let file = tokio::task::spawn_blocking(move || {
        file.lock_exclusive().map_err(|err| {
            anyhow::anyhow!("failed to acquire exclusive validation lease: {err}")
        })?;
        Result::<fs::File>::Ok(file)
    })
    .await
    .context("exclusive validation lease task panicked")??;
    Ok(ExclusiveValidationLease {
        _file: file,
        waited: started.elapsed(),
    })
}
