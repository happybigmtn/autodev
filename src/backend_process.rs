use std::fs;
use std::path::Path;

use anyhow::{Context, Result};
use tokio::io::{AsyncRead, AsyncReadExt, BufReader};

use crate::util::{atomic_write, timestamp_slug};

pub(crate) fn write_worker_pid(worker_pid_path: Option<&Path>, pid: Option<u32>) -> Result<()> {
    let Some(path) = worker_pid_path else {
        return Ok(());
    };
    let Some(pid) = pid else {
        return Ok(());
    };
    atomic_write(path, pid.to_string().as_bytes())
        .with_context(|| format!("failed to write {}", path.display()))
}

pub(crate) fn clear_worker_pid(worker_pid_path: Option<&Path>) -> Result<()> {
    let Some(path) = worker_pid_path else {
        return Ok(());
    };
    if !path.exists() {
        return Ok(());
    }
    fs::remove_file(path).with_context(|| format!("failed to remove {}", path.display()))
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

#[cfg(test)]
mod tests {
    use std::fs;

    use super::{clear_worker_pid, log_stderr, write_worker_pid};
    use crate::util::timestamp_slug;

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
    fn worker_pid_round_trips_then_clears() {
        let path = std::env::temp_dir().join(format!("backend-process-pid-{}", timestamp_slug()));
        write_worker_pid(Some(&path), Some(4242)).expect("write worker pid");
        assert_eq!(fs::read_to_string(&path).expect("read worker pid"), "4242");
        clear_worker_pid(Some(&path)).expect("clear worker pid");
        assert!(!path.exists());
        clear_worker_pid(Some(&path)).expect("clearing a missing pid file is a no-op");
    }
}
