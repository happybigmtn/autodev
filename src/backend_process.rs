use std::fs;
#[cfg(unix)]
use std::io::Write;
use std::path::{Path, PathBuf};
#[cfg(unix)]
use std::sync::atomic::{AtomicU64, Ordering};
#[cfg(unix)]
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use tokio::io::{AsyncRead, AsyncReadExt, BufReader};

use crate::util::{atomic_write, timestamp_slug};

#[cfg(unix)]
static WORKER_PID_LEASE_SEQUENCE: AtomicU64 = AtomicU64::new(0);
#[cfg(unix)]
const WORKER_PID_LEASE_PREFIX: &str = ".auto-worker-pid-lease-";

#[derive(Debug)]
pub(crate) struct WorkerPidGuard {
    lease_path: Option<PathBuf>,
}

impl WorkerPidGuard {
    pub(crate) fn new(worker_pid_path: Option<&Path>, pid: Option<u32>) -> Result<Self> {
        let (Some(path), Some(pid)) = (worker_pid_path, pid) else {
            return Ok(Self { lease_path: None });
        };

        #[cfg(unix)]
        let lease_path = publish_worker_pid_lease(path, pid)?;
        #[cfg(not(unix))]
        let lease_path = {
            atomic_write(path, pid.to_string().as_bytes())
                .with_context(|| format!("failed to write {}", path.display()))?;
            path.to_path_buf()
        };

        Ok(Self {
            lease_path: Some(lease_path),
        })
    }

    pub(crate) fn clear(self) -> Result<()> {
        self.clear_with(retire_worker_pid_lease)
    }

    fn clear_with<F>(mut self, remove: F) -> Result<()>
    where
        F: FnOnce(&Path) -> Result<()>,
    {
        // Taking the lease before attempting I/O is the disarm point. Even if
        // explicit cleanup fails, Drop cannot issue a second unlink.
        let Some(lease_path) = self.lease_path.take() else {
            return Ok(());
        };
        remove(&lease_path)
    }
}

impl Drop for WorkerPidGuard {
    fn drop(&mut self) {
        if let Some(lease_path) = self.lease_path.take() {
            let _ = retire_worker_pid_lease(&lease_path);
        }
    }
}

#[cfg(unix)]
fn publish_worker_pid_lease(path: &Path, pid: u32) -> Result<PathBuf> {
    use std::os::unix::fs::{symlink, OpenOptionsExt};

    let parent = path
        .parent()
        .with_context(|| format!("{} has no parent directory", path.display()))?;
    fs::create_dir_all(parent).with_context(|| format!("failed to create {}", parent.display()))?;
    let nonce_time = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();

    for _ in 0..1024 {
        let sequence = WORKER_PID_LEASE_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let nonce = format!("{}-{pid}-{nonce_time}-{sequence}", std::process::id());
        let lease_name = format!("{WORKER_PID_LEASE_PREFIX}{nonce}");
        let lease_path = parent.join(&lease_name);
        let mut lease = match fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .mode(0o600)
            .open(&lease_path)
        {
            Ok(lease) => lease,
            Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(err) => {
                return Err(err)
                    .with_context(|| format!("failed to create {}", lease_path.display()));
            }
        };
        if let Err(err) = lease.write_all(pid.to_string().as_bytes()) {
            let _ = fs::remove_file(&lease_path);
            return Err(err).with_context(|| format!("failed to write {}", lease_path.display()));
        }
        drop(lease);

        let publication_path = parent.join(format!(".auto-worker-pid-publish-{nonce}"));
        if let Err(err) = symlink(&lease_name, &publication_path) {
            let _ = fs::remove_file(&lease_path);
            if err.kind() == std::io::ErrorKind::AlreadyExists {
                continue;
            }
            return Err(err).with_context(|| {
                format!(
                    "failed to create worker pid publication {}",
                    publication_path.display()
                )
            });
        }
        if let Err(err) = fs::rename(&publication_path, path) {
            let _ = fs::remove_file(&publication_path);
            let _ = fs::remove_file(&lease_path);
            return Err(err).with_context(|| {
                format!("failed to publish worker pid lease at {}", path.display())
            });
        }
        return Ok(lease_path);
    }

    anyhow::bail!(
        "failed to allocate a unique worker pid lease beside {} after 1024 attempts",
        path.display()
    )
}

pub(crate) fn retire_worker_pid_lease(path: &Path) -> Result<()> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(err) => Err(err).with_context(|| format!("failed to remove {}", path.display())),
    }
}

#[cfg(unix)]
pub(crate) fn worker_pid_lease_target(path: &Path) -> Result<Option<PathBuf>> {
    use std::path::Component;

    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(err) => {
            return Err(err).with_context(|| format!("failed to inspect {}", path.display()));
        }
    };
    if !metadata.file_type().is_symlink() {
        return Ok(None);
    }
    let target = fs::read_link(path)
        .with_context(|| format!("failed to read worker pid publication {}", path.display()))?;
    let mut components = target.components();
    let Some(Component::Normal(file_name)) = components.next() else {
        anyhow::bail!(
            "worker pid publication {} has a non-local lease target {}",
            path.display(),
            target.display()
        );
    };
    if components.next().is_some()
        || !file_name
            .to_string_lossy()
            .starts_with(WORKER_PID_LEASE_PREFIX)
    {
        anyhow::bail!(
            "worker pid publication {} has an invalid lease target {}",
            path.display(),
            target.display()
        );
    }
    let parent = path
        .parent()
        .with_context(|| format!("{} has no parent directory", path.display()))?;
    Ok(Some(parent.join(file_name)))
}

#[cfg(not(unix))]
pub(crate) fn worker_pid_lease_target(_path: &Path) -> Result<Option<PathBuf>> {
    Ok(None)
}

pub(crate) fn log_stderr(stderr_text: &str, stderr_log_path: &Path) -> Result<()> {
    let rendered = if stderr_text.trim().is_empty() {
        "[no stderr captured]"
    } else {
        stderr_text
    };
    let entry = format!("\n===== {} =====\n{rendered}\n", timestamp_slug());
    let mut existing = if stderr_log_path.exists() {
        fs::read(stderr_log_path)
            .with_context(|| format!("failed to read {}", stderr_log_path.display()))?
    } else {
        Vec::new()
    };
    existing.extend_from_slice(entry.as_bytes());
    atomic_write(stderr_log_path, &existing)?;
    Ok(())
}

pub(crate) async fn read_stream<R>(stream: R) -> Result<String>
where
    R: AsyncRead + Unpin,
{
    let mut reader = BufReader::new(stream);
    let mut text = String::new();
    reader
        .read_to_string(&mut text)
        .await
        .context("failed to read child stream")?;
    Ok(text)
}

pub(crate) async fn read_stream_bytes<R>(stream: R) -> std::io::Result<Vec<u8>>
where
    R: AsyncRead + Unpin,
{
    let mut reader = BufReader::new(stream);
    let mut bytes = Vec::new();
    reader.read_to_end(&mut bytes).await?;
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;
    use std::fs;
    use std::path::Path;

    use super::{log_stderr, WorkerPidGuard};
    use crate::util::timestamp_slug;
    use anyhow::anyhow;

    fn remove_test_publication(path: &Path) {
        let _ = fs::remove_file(path);
    }

    #[test]
    fn empty_stderr_still_writes_artifact() {
        let path =
            std::env::temp_dir().join(format!("backend-process-stderr-{}.log", timestamp_slug()));
        log_stderr("", &path).expect("write stderr log");
        let written = fs::read_to_string(&path).expect("read stderr log");
        assert!(written.contains("[no stderr captured]"));
        let _ = fs::remove_file(path);
    }

    #[test]
    fn worker_pid_guard_drop_clears_its_owned_file() {
        let path =
            std::env::temp_dir().join(format!("backend-process-pid-drop-{}", timestamp_slug()));
        {
            let _guard = WorkerPidGuard::new(Some(&path), Some(4242)).expect("write worker pid");
            assert_eq!(fs::read_to_string(&path).expect("read worker pid"), "4242");
        }
        assert!(!path.exists());
        remove_test_publication(&path);
    }

    #[test]
    fn worker_pid_guard_explicit_clear_removes_and_disarms() {
        let path =
            std::env::temp_dir().join(format!("backend-process-pid-clear-{}", timestamp_slug()));
        let guard = WorkerPidGuard::new(Some(&path), Some(4242)).expect("write worker pid");

        guard.clear().expect("clear worker pid");

        assert!(!path.exists());
        #[cfg(unix)]
        {
            let metadata =
                fs::symlink_metadata(&path).expect("dangling pid publication should remain");
            assert!(metadata.file_type().is_symlink());
            let err = fs::read_to_string(&path).expect_err("cleared lease must be unreadable");
            assert_eq!(err.kind(), std::io::ErrorKind::NotFound);
        }
        remove_test_publication(&path);
    }

    #[cfg(unix)]
    #[test]
    fn worker_pid_guard_publishes_an_owner_unique_lease() {
        let path =
            std::env::temp_dir().join(format!("backend-process-pid-lease-{}", timestamp_slug()));
        let guard = WorkerPidGuard::new(Some(&path), Some(4242)).expect("write worker pid");

        let lease_name = fs::read_link(&path).expect("worker pid publication should be a symlink");
        assert_eq!(
            lease_name.components().count(),
            1,
            "lease target should stay in the worker pid directory"
        );
        assert_eq!(fs::read_to_string(&path).expect("read worker pid"), "4242");

        guard.clear().expect("clear worker pid");
        assert!(!path.exists());
        remove_test_publication(&path);
    }

    #[cfg(unix)]
    #[test]
    fn older_guard_cleanup_cannot_unlink_a_newer_publication() {
        let path =
            std::env::temp_dir().join(format!("backend-process-pid-handoff-{}", timestamp_slug()));
        let older = WorkerPidGuard::new(Some(&path), Some(4242)).expect("write older worker pid");
        let older_lease = fs::read_link(&path).expect("read older lease");
        let newer = WorkerPidGuard::new(Some(&path), Some(9001)).expect("write newer worker pid");
        let newer_lease = fs::read_link(&path).expect("read newer lease");
        assert_ne!(older_lease, newer_lease);

        older.clear().expect("clear older worker pid");

        assert_eq!(
            fs::read_to_string(&path).expect("read newer worker pid"),
            "9001"
        );
        assert!(!path
            .parent()
            .expect("worker pid parent")
            .join(older_lease)
            .exists());
        assert!(path
            .parent()
            .expect("worker pid parent")
            .join(newer_lease)
            .exists());

        newer.clear().expect("clear newer worker pid");
        assert!(!path.exists());
        remove_test_publication(&path);
    }

    #[test]
    fn explicit_clear_attempts_cleanup_exactly_once_when_cleanup_fails() {
        let path = std::env::temp_dir().join(format!(
            "backend-process-pid-clear-error-{}",
            timestamp_slug()
        ));
        let guard = WorkerPidGuard::new(Some(&path), Some(4242)).expect("write worker pid");
        #[cfg(unix)]
        let lease_path = path
            .parent()
            .expect("worker pid parent")
            .join(fs::read_link(&path).expect("read worker pid lease"));
        let attempts = Cell::new(0_u8);

        let result = guard.clear_with(|_lease_path: &Path| {
            attempts.set(attempts.get() + 1);
            Err(anyhow!("injected cleanup failure"))
        });

        assert!(result.is_err());
        assert_eq!(
            attempts.get(),
            1,
            "Drop must not retry after an explicit cleanup attempt"
        );
        let _ = fs::remove_file(path);
        #[cfg(unix)]
        let _ = fs::remove_file(lease_path);
    }

    #[test]
    fn worker_pid_guard_preserves_a_replacement_owner() {
        let path = std::env::temp_dir().join(format!(
            "backend-process-pid-replacement-{}",
            timestamp_slug()
        ));
        let guard = WorkerPidGuard::new(Some(&path), Some(4242)).expect("write worker pid");
        let replacement_path = path.with_extension("replacement");
        fs::write(&replacement_path, "9001").expect("write replacement worker pid");
        fs::rename(&replacement_path, &path).expect("publish replacement worker pid ownership");

        drop(guard);

        assert_eq!(
            fs::read_to_string(&path).expect("read replacement worker pid"),
            "9001"
        );
        let _ = fs::remove_file(path);
    }

    #[tokio::test]
    async fn cancelling_the_owner_future_drops_and_clears_the_pid_guard() {
        let path =
            std::env::temp_dir().join(format!("backend-process-pid-cancel-{}", timestamp_slug()));
        let task_path = path.clone();
        let (ready_tx, ready_rx) = tokio::sync::oneshot::channel();
        let task = tokio::spawn(async move {
            let _guard =
                WorkerPidGuard::new(Some(&task_path), Some(4242)).expect("write worker pid");
            let _ = ready_tx.send(());
            std::future::pending::<()>().await;
        });
        ready_rx.await.expect("pid guard should be armed");
        assert!(path.exists());

        task.abort();
        let _ = task.await;

        assert!(!path.exists());
        remove_test_publication(&path);
    }
}
