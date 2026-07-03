use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::UNIX_EPOCH;

use anyhow::{bail, Context, Result};
use chrono::Utc;

const AUTO_LOG_KEEP_FILES: usize = 64;
const AUTO_FRESH_INPUT_KEEP_ENTRIES: usize = 12;
const AUTO_QUEUE_RUN_KEEP_ENTRIES: usize = 12;
const PI_RUNTIME_LOG_KEEP_FILES: usize = 5;
const PI_RUNTIME_LOG_MAX_BYTES: usize = 2 * 1024 * 1024;
static ATOMIC_WRITE_TEMP_SEQUENCE: AtomicU64 = AtomicU64::new(0);

pub(crate) fn ensure_repo_layout(repo_root: &Path) -> Result<()> {
    ensure_repo_layout_with(repo_root, prune_old_entries, prune_pi_runtime_state)
}

fn ensure_repo_layout_with<F, G>(
    repo_root: &Path,
    mut prune_entries: F,
    mut prune_pi_state: G,
) -> Result<()>
where
    F: FnMut(&Path, usize) -> Result<()>,
    G: FnMut(&Path) -> Result<()>,
{
    for rel in [
        ".auto",
        ".auto/fresh-input",
        ".auto/logs",
        ".auto/queue-runs",
    ] {
        let path = repo_root.join(rel);
        fs::create_dir_all(&path)
            .with_context(|| format!("failed to create {}", path.display()))?;
    }

    let mut failures = Vec::new();
    for (path, keep) in [
        (repo_root.join(".auto").join("logs"), AUTO_LOG_KEEP_FILES),
        (
            repo_root.join(".auto").join("fresh-input"),
            AUTO_FRESH_INPUT_KEEP_ENTRIES,
        ),
        (
            repo_root.join(".auto").join("queue-runs"),
            AUTO_QUEUE_RUN_KEEP_ENTRIES,
        ),
    ] {
        if let Err(err) = prune_entries(&path, keep) {
            eprintln!("warning: failed to prune {}: {err}", path.display());
            failures.push(format!("{}: {err}", path.display()));
        }
    }

    if let Err(err) = prune_pi_state(repo_root) {
        let agent_dir = opencode_agent_dir(repo_root);
        eprintln!(
            "warning: failed to prune PI runtime state in {}: {err}",
            agent_dir.display()
        );
        failures.push(format!("{}: {err}", agent_dir.display()));
    }
    if !failures.is_empty() {
        bail!(
            "failed to finish repo layout pruning:\n- {}",
            failures.join("\n- ")
        );
    }
    Ok(())
}

#[cfg(unix)]
pub(crate) fn chmod_0o600_if_unix(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;

    let mut permissions = fs::metadata(path)
        .with_context(|| format!("failed to stat {}", path.display()))?
        .permissions();
    permissions.set_mode(0o600);
    fs::set_permissions(path, permissions)
        .with_context(|| format!("failed to set owner-only permissions on {}", path.display()))?;
    Ok(())
}

#[cfg(not(unix))]
pub(crate) fn chmod_0o600_if_unix(_path: &Path) -> Result<()> {
    Ok(())
}

#[cfg(unix)]
pub(crate) fn write_0o600_if_unix(path: &Path, bytes: &[u8]) -> Result<()> {
    use std::os::unix::fs::OpenOptionsExt;

    if path.exists() {
        chmod_0o600_if_unix(path)?;
    }
    let mut file = fs::OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .mode(0o600)
        .open(path)
        .with_context(|| format!("failed to open {}", path.display()))?;
    file.write_all(bytes)
        .with_context(|| format!("failed to write {}", path.display()))?;
    chmod_0o600_if_unix(path)
}

#[cfg(not(unix))]
pub(crate) fn write_0o600_if_unix(path: &Path, bytes: &[u8]) -> Result<()> {
    fs::write(path, bytes).with_context(|| format!("failed to write {}", path.display()))
}

#[cfg(test)]
pub(crate) fn test_process_env_lock() -> &'static std::sync::Mutex<()> {
    static LOCK: std::sync::OnceLock<std::sync::Mutex<()>> = std::sync::OnceLock::new();
    LOCK.get_or_init(|| std::sync::Mutex::new(()))
}

pub(crate) fn atomic_write(path: &Path, bytes: &[u8]) -> Result<()> {
    let parent = path
        .parent()
        .with_context(|| format!("{} has no parent directory", path.display()))?;
    fs::create_dir_all(parent).with_context(|| format!("failed to create {}", parent.display()))?;
    let temp = atomic_write_temp_path(parent, path);
    fs::write(&temp, bytes).map_err(|err| {
        atomic_write_failure(err, &temp, format!("failed to write {}", temp.display()))
    })?;
    atomic_rename(&temp, path)
}

pub(crate) fn ensure_writable_run_root(run_root: &Path) -> Result<()> {
    fs::create_dir_all(run_root).with_context(|| run_root_error_context(run_root, "create"))?;
    let metadata = fs::metadata(run_root)
        .with_context(|| run_root_error_context(run_root, "stat after create"))?;
    if !metadata.is_dir() {
        bail!("{}", run_root_error_context(run_root, "use as directory"));
    }
    let probe = run_root.join(format!(
        ".auto-run-root-write-test-{}-{}",
        std::process::id(),
        ATOMIC_WRITE_TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed)
    ));
    let write_result = fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&probe)
        .and_then(|mut file| file.write_all(b"ok"));
    if let Err(err) = write_result {
        return Err(err).with_context(|| run_root_error_context(run_root, "write probe in"));
    }
    if let Err(err) = fs::remove_file(&probe) {
        return Err(err).with_context(|| {
            format!(
                "failed to remove run-root write probe {} after preparing {}",
                probe.display(),
                run_root.display()
            )
        });
    }
    Ok(())
}

fn run_root_error_context(run_root: &Path, action: &str) -> String {
    let mut message = format!("failed to {action} run root {}", run_root.display());
    if let Some(value) = auto_run_root_env_value_for_path(run_root) {
        message.push_str(&format!(
            "; AUTO_RUN_ROOT={} resolved this path. Unset AUTO_RUN_ROOT, update it to an existing writable mount, or create/chown the directory.",
            value.display()
        ));
    }
    message
}

fn auto_run_root_env_value_for_path(path: &Path) -> Option<PathBuf> {
    let raw = std::env::var_os("AUTO_RUN_ROOT")?;
    let raw_path = PathBuf::from(raw);
    let raw_text = raw_path.to_string_lossy();
    if raw_text.trim().is_empty() {
        return None;
    }
    path.starts_with(&raw_path).then_some(raw_path)
}

#[cfg(unix)]
pub(crate) fn atomic_write_0o600_if_unix(path: &Path, bytes: &[u8]) -> Result<()> {
    use std::os::unix::fs::OpenOptionsExt;

    reject_symlink_destination(path)?;
    let parent = path
        .parent()
        .with_context(|| format!("{} has no parent directory", path.display()))?;
    fs::create_dir_all(parent).with_context(|| format!("failed to create {}", parent.display()))?;
    let temp = atomic_write_temp_path(parent, path);
    let write_result = (|| -> std::io::Result<()> {
        let mut file = fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .mode(0o600)
            .open(&temp)?;
        file.write_all(bytes)?;
        file.sync_all()
    })();
    if let Err(err) = write_result {
        return Err(atomic_write_failure(
            err,
            &temp,
            format!("failed to write {}", temp.display()),
        ));
    }
    atomic_rename(&temp, path)
}

#[cfg(not(unix))]
pub(crate) fn atomic_write_0o600_if_unix(path: &Path, bytes: &[u8]) -> Result<()> {
    reject_symlink_destination(path)?;
    atomic_write(path, bytes)
}

fn atomic_write_temp_path(parent: &Path, path: &Path) -> std::path::PathBuf {
    parent.join(format!(
        ".{}.tmp-{}-{}-{}",
        path.file_name().and_then(|v| v.to_str()).unwrap_or("write"),
        std::process::id(),
        Utc::now().timestamp_nanos_opt().unwrap_or_default(),
        ATOMIC_WRITE_TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed)
    ))
}

fn atomic_rename(temp: &Path, path: &Path) -> Result<()> {
    fs::rename(temp, path).map_err(|err| {
        atomic_write_failure(
            err,
            temp,
            format!("failed to atomically replace {}", path.display()),
        )
    })?;
    Ok(())
}

fn reject_symlink_destination(path: &Path) -> Result<()> {
    match fs::symlink_metadata(path) {
        Ok(meta) if meta.file_type().is_symlink() => {
            anyhow::bail!(
                "refusing to replace symlinked destination {}",
                path.display()
            )
        }
        Ok(_) => Ok(()),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(err) => Err(err).with_context(|| format!("failed to stat {}", path.display())),
    }
}

fn atomic_write_failure(error: std::io::Error, temp: &Path, context: String) -> anyhow::Error {
    let cleanup_error = match fs::remove_file(temp) {
        Ok(()) => None,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => None,
        Err(err) => Some(err),
    };
    let mut message = context;
    if let Some(err) = cleanup_error {
        message.push_str(&format!(
            "; also failed to remove temp {}: {}",
            temp.display(),
            err
        ));
    }
    anyhow::Error::new(error).context(message)
}

pub(crate) fn copy_tree(src: &Path, dst: &Path) -> Result<()> {
    if src.is_file() {
        if let Some(parent) = dst.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("failed to create {}", parent.display()))?;
        }
        fs::copy(src, dst)
            .with_context(|| format!("failed to copy {} -> {}", src.display(), dst.display()))?;
        return Ok(());
    }

    fs::create_dir_all(dst).with_context(|| format!("failed to create {}", dst.display()))?;
    for entry in fs::read_dir(src).with_context(|| format!("failed to read {}", src.display()))? {
        let entry = entry?;
        let child_src = entry.path();
        let child_dst = dst.join(entry.file_name());
        if child_src.is_dir() {
            copy_tree(&child_src, &child_dst)?;
        } else {
            fs::copy(&child_src, &child_dst).with_context(|| {
                format!(
                    "failed to copy {} -> {}",
                    child_src.display(),
                    child_dst.display()
                )
            })?;
        }
    }
    Ok(())
}

pub(crate) fn list_markdown_files(dir: &Path) -> Result<Vec<PathBuf>> {
    let mut files = Vec::new();
    if !dir.exists() {
        return Ok(files);
    }
    collect_markdown_files(dir, &mut files)?;
    files.sort();
    Ok(files)
}

fn collect_markdown_files(dir: &Path, files: &mut Vec<PathBuf>) -> Result<()> {
    for entry in fs::read_dir(dir).with_context(|| format!("failed to read {}", dir.display()))? {
        let entry = entry?;
        let path = entry.path();
        if path.is_dir() {
            collect_markdown_files(&path, files)?;
        } else if path.extension().and_then(|ext| ext.to_str()) == Some("md") {
            files.push(path);
        }
    }
    Ok(())
}

pub(crate) fn prune_old_entries(dir: &Path, keep: usize) -> Result<()> {
    if !dir.exists() {
        return Ok(());
    }
    if keep == 0 {
        clear_dir_contents(dir)?;
        return Ok(());
    }

    let mut entries = fs::read_dir(dir)
        .with_context(|| format!("failed to read {}", dir.display()))?
        .collect::<std::result::Result<Vec<_>, _>>()
        .with_context(|| format!("failed to read {}", dir.display()))?
        .into_iter()
        .map(|entry| {
            let path = entry.path();
            let modified = entry
                .metadata()
                .and_then(|meta| meta.modified())
                .unwrap_or(UNIX_EPOCH);
            (modified, path)
        })
        .collect::<Vec<_>>();
    if entries.len() <= keep {
        return Ok(());
    }

    entries.sort_by_key(|(modified, path)| (*modified, path.clone()));
    let remove_count = entries.len().saturating_sub(keep);
    for (_, path) in entries.into_iter().take(remove_count) {
        remove_path(&path)?;
    }
    Ok(())
}

pub(crate) fn truncate_file_to_max_bytes(path: &Path, max_bytes: usize) -> Result<()> {
    if !path.exists() || max_bytes == 0 {
        return Ok(());
    }
    let bytes = fs::read(path).with_context(|| format!("failed to read {}", path.display()))?;
    if bytes.len() <= max_bytes {
        return Ok(());
    }
    let keep_from = bytes.len().saturating_sub(max_bytes);
    atomic_write(path, &bytes[keep_from..])?;
    Ok(())
}

pub(crate) fn opencode_agent_dir(repo_root: &Path) -> PathBuf {
    repo_root
        .join(".auto")
        .join("opencode-data")
        .join("opencode")
}

pub(crate) fn prune_pi_runtime_state(repo_root: &Path) -> Result<()> {
    let agent_dir = opencode_agent_dir(repo_root);
    if !agent_dir.exists() {
        return Ok(());
    }

    let log_dir = agent_dir.join("log");
    if log_dir.exists() {
        prune_old_entries(&log_dir, PI_RUNTIME_LOG_KEEP_FILES)?;
        for entry in fs::read_dir(&log_dir)
            .with_context(|| format!("failed to read {}", log_dir.display()))?
        {
            let path = entry?.path();
            if path.is_file() {
                truncate_file_to_max_bytes(&path, PI_RUNTIME_LOG_MAX_BYTES)?;
            }
        }
    }

    clear_and_recreate_dir(&agent_dir.join("snapshot"))?;
    clear_and_recreate_dir(&agent_dir.join("storage").join("session_diff"))?;
    Ok(())
}

pub(crate) fn clear_and_recreate_dir(path: &Path) -> Result<()> {
    if path.exists() {
        fs::remove_dir_all(path).with_context(|| format!("failed to clear {}", path.display()))?;
    }
    fs::create_dir_all(path).with_context(|| format!("failed to create {}", path.display()))?;
    Ok(())
}

fn clear_dir_contents(dir: &Path) -> Result<()> {
    for entry in fs::read_dir(dir).with_context(|| format!("failed to read {}", dir.display()))? {
        let path = entry?.path();
        remove_path(&path)?;
    }
    Ok(())
}

fn remove_path(path: &Path) -> Result<()> {
    if path.is_dir() {
        fs::remove_dir_all(path).with_context(|| format!("failed to remove {}", path.display()))?;
    } else {
        fs::remove_file(path).with_context(|| format!("failed to remove {}", path.display()))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::fs;
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;
    use std::path::PathBuf;
    use std::sync::{mpsc, Arc, Barrier};
    use std::thread::sleep;
    use std::time::{Duration, SystemTime, UNIX_EPOCH};

    use super::{
        atomic_write, atomic_write_0o600_if_unix, chmod_0o600_if_unix, ensure_repo_layout_with,
        ensure_writable_run_root, prune_old_entries, test_process_env_lock,
        truncate_file_to_max_bytes,
        write_0o600_if_unix,
    };

    fn temp_repo_path(name: &str) -> PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system time should be after unix epoch")
            .as_nanos();
        std::env::temp_dir().join(format!("autodev-{name}-{}-{nonce}", std::process::id()))
    }

    #[test]
    fn truncate_file_to_max_bytes_keeps_tail() {
        let dir = temp_repo_path("truncate-file");
        fs::create_dir_all(&dir).expect("failed to create temp dir");
        let path = dir.join("log.txt");
        fs::write(&path, b"abcdefghij").expect("failed to write log");

        truncate_file_to_max_bytes(&path, 4).expect("failed to truncate file");

        let text = fs::read_to_string(&path).expect("failed to read log");
        assert_eq!(text, "ghij");
        fs::remove_dir_all(&dir).expect("failed to remove temp dir");
    }

    #[test]
    fn prune_old_entries_keeps_latest_paths() {
        let dir = temp_repo_path("prune-old-entries");
        fs::create_dir_all(&dir).expect("failed to create temp dir");
        let first = dir.join("one.txt");
        let second = dir.join("two.txt");
        let third = dir.join("three.txt");

        fs::write(&first, "one").expect("failed to write first");
        sleep(Duration::from_millis(5));
        fs::write(&second, "two").expect("failed to write second");
        sleep(Duration::from_millis(5));
        fs::write(&third, "three").expect("failed to write third");

        prune_old_entries(&dir, 2).expect("failed to prune entries");

        assert!(!first.exists());
        assert!(second.exists());
        assert!(third.exists());
        fs::remove_dir_all(&dir).expect("failed to remove temp dir");
    }

    #[test]
    fn atomic_write_works_outside_git_repo() {
        let dir = temp_repo_path("atomic-write-non-git");
        fs::create_dir_all(&dir).expect("failed to create temp dir");
        let target = dir.join("state.json");
        let payload = br#"{"outside":"git"}"#;

        assert!(
            !dir.join(".git").exists(),
            "fixture should stay outside a git repo"
        );

        atomic_write(&target, payload).expect("atomic write should succeed outside a git repo");

        let written = fs::read(&target).expect("failed to read atomic write output");
        assert_eq!(written, payload);

        fs::remove_dir_all(&dir).expect("failed to remove temp dir");
    }

    #[test]
    fn ensure_writable_run_root_names_auto_run_root_on_failure() {
        let _guard = test_process_env_lock().lock().expect("env lock poisoned");
        let previous = std::env::var_os("AUTO_RUN_ROOT");
        let dir = temp_repo_path("run-root-file");
        fs::create_dir_all(&dir).expect("failed to create temp dir");
        let auto_root = dir.join("auto-runs");
        fs::write(&auto_root, "not a directory").expect("failed to write file");
        std::env::set_var("AUTO_RUN_ROOT", &auto_root);
        let run_root = auto_root.join("repo").join("parallel");

        let err = ensure_writable_run_root(&run_root).expect_err("file root should fail");
        let message = format!("{err:#}");
        assert!(message.contains("AUTO_RUN_ROOT"), "{message}");
        assert!(message.contains(&run_root.display().to_string()), "{message}");
        assert!(
            message.contains("Unset AUTO_RUN_ROOT") || message.contains("update it"),
            "{message}"
        );

        match previous {
            Some(value) => std::env::set_var("AUTO_RUN_ROOT", value),
            None => std::env::remove_var("AUTO_RUN_ROOT"),
        }
        fs::remove_dir_all(&dir).expect("failed to remove temp dir");
    }

    #[test]
    fn atomic_write_creates_missing_parent_dir() {
        let dir = temp_repo_path("atomic-write-missing-parent");
        fs::create_dir_all(&dir).expect("failed to create temp dir");
        let target = dir.join("nested").join("missing").join("result.json");
        let parent = target.parent().expect("target should have a parent");
        let payload = br#"{"created":"parent"}"#;

        assert!(!parent.exists(), "parent should start missing");

        atomic_write(&target, payload).expect("atomic write should create missing parents");

        assert!(
            parent.is_dir(),
            "atomic write should create the parent directory"
        );
        let written = fs::read(&target).expect("failed to read atomic write output");
        assert_eq!(written, payload);

        fs::remove_dir_all(&dir).expect("failed to remove temp dir");
    }

    #[test]
    fn atomic_write_handles_rapid_succession_collisions() {
        let dir = temp_repo_path("atomic-write-collision");
        fs::create_dir_all(&dir).expect("failed to create temp dir");
        let target = dir.join("state.json");
        let concurrent_writers = 3usize;
        let start = Arc::new(Barrier::new(concurrent_writers + 1));
        let (done_tx, done_rx) = mpsc::channel();
        let mut handles = Vec::new();

        for writer in 0..concurrent_writers {
            let start = Arc::clone(&start);
            let done_tx = done_tx.clone();
            let target = target.clone();
            handles.push(std::thread::spawn(move || -> anyhow::Result<()> {
                let mut payload = format!("writer-{writer}:").into_bytes();
                payload.resize(128 * 1024, b'a' + writer as u8);

                start.wait();
                let result = atomic_write(&target, &payload);
                done_tx
                    .send(())
                    .expect("failed to signal concurrent writer completion");
                result
            }));
        }
        drop(done_tx);

        let final_payload = {
            let mut payload = b"writer-final:".to_vec();
            payload.resize(128 * 1024, b'z');
            payload
        };
        let start_for_final = Arc::clone(&start);
        let target_for_final = target.clone();
        handles.push(std::thread::spawn(move || -> anyhow::Result<()> {
            start_for_final.wait();
            for _ in 0..concurrent_writers {
                done_rx
                    .recv()
                    .expect("failed to wait for concurrent writer completion");
            }
            atomic_write(&target_for_final, &final_payload)
        }));

        for handle in handles {
            handle
                .join()
                .expect("writer thread should not panic")
                .expect("all atomic writes should succeed");
        }

        let written = fs::read(&target).expect("failed to read atomic write output");
        let mut expected = b"writer-final:".to_vec();
        expected.resize(128 * 1024, b'z');
        assert_eq!(written, expected);

        let temp_files = fs::read_dir(&dir)
            .expect("failed to read temp dir")
            .map(|entry| {
                entry
                    .expect("failed to read temp dir entry")
                    .file_name()
                    .to_string_lossy()
                    .into_owned()
            })
            .filter(|name| name.starts_with(".state.json.tmp-"))
            .collect::<Vec<_>>();
        assert!(
            temp_files.is_empty(),
            "unexpected temp files after concurrent writes: {temp_files:?}"
        );

        fs::remove_dir_all(&dir).expect("failed to remove temp dir");
    }

    #[test]
    fn atomic_write_removes_temp_file_after_rename_failure() {
        let dir = temp_repo_path("atomic-write-cleanup");
        fs::create_dir_all(&dir).expect("failed to create temp dir");
        let target = dir.join("result.json");
        fs::create_dir_all(&target).expect("failed to create conflicting target directory");

        let err = atomic_write(&target, br#"{"ok":true}"#)
            .expect_err("renaming a file over a directory should fail");
        assert!(err.to_string().contains("failed to atomically replace"));

        let mut entries = fs::read_dir(&dir)
            .expect("failed to read temp dir")
            .map(|entry| {
                entry
                    .expect("failed to read temp dir entry")
                    .file_name()
                    .to_string_lossy()
                    .into_owned()
            })
            .collect::<Vec<_>>();
        entries.sort();
        assert_eq!(entries, vec!["result.json".to_string()]);

        fs::remove_dir_all(&dir).expect("failed to remove temp dir");
    }

    #[cfg(unix)]
    #[test]
    fn atomic_write_0o600_if_unix_preserves_owner_only_mode() {
        let dir = temp_repo_path("atomic-write-0600");
        fs::create_dir_all(&dir).expect("failed to create temp dir");
        let target = dir.join("state.json");
        fs::write(&target, br#"{"old":true}"#).expect("failed to seed target");

        let mut permissions = fs::metadata(&target)
            .expect("failed to stat seeded target")
            .permissions();
        permissions.set_mode(0o644);
        fs::set_permissions(&target, permissions).expect("failed to loosen target permissions");

        atomic_write_0o600_if_unix(&target, br#"{"new":true}"#)
            .expect("atomic owner-only write should succeed");

        assert_eq!(
            fs::read(&target).expect("failed to read target"),
            br#"{"new":true}"#
        );
        let mode = fs::metadata(&target)
            .expect("failed to stat target")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o600);

        let temp_files = fs::read_dir(&dir)
            .expect("failed to read temp dir")
            .map(|entry| {
                entry
                    .expect("failed to read temp dir entry")
                    .file_name()
                    .to_string_lossy()
                    .into_owned()
            })
            .filter(|name| name.starts_with(".state.json.tmp-"))
            .collect::<Vec<_>>();
        assert!(
            temp_files.is_empty(),
            "unexpected temp files after owner-only write: {temp_files:?}"
        );

        fs::remove_dir_all(&dir).expect("failed to remove temp dir");
    }

    #[cfg(unix)]
    #[test]
    fn chmod_0o600_if_unix_sets_owner_only_mode() {
        let dir = temp_repo_path("chmod-0600");
        fs::create_dir_all(&dir).expect("failed to create temp dir");
        let path = dir.join("credentials.json");
        fs::write(&path, br#"{"token":"secret"}"#).expect("failed to seed credential file");

        let mut permissions = fs::metadata(&path)
            .expect("failed to stat credential file")
            .permissions();
        permissions.set_mode(0o644);
        fs::set_permissions(&path, permissions).expect("failed to loosen credential permissions");

        chmod_0o600_if_unix(&path).expect("chmod helper should succeed");

        let mode = fs::metadata(&path)
            .expect("failed to stat credential file")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o600);

        fs::remove_dir_all(&dir).expect("failed to remove temp dir");
    }

    #[cfg(unix)]
    #[test]
    fn write_0o600_if_unix_tightens_existing_file_before_write() {
        let dir = temp_repo_path("write-0600");
        fs::create_dir_all(&dir).expect("failed to create temp dir");
        let path = dir.join("credentials.json");
        fs::write(&path, br#"{"token":"old"}"#).expect("failed to seed credential file");

        let mut permissions = fs::metadata(&path)
            .expect("failed to stat credential file")
            .permissions();
        permissions.set_mode(0o644);
        fs::set_permissions(&path, permissions).expect("failed to loosen credential permissions");

        write_0o600_if_unix(&path, br#"{"token":"new"}"#).expect("owner-only write should succeed");

        assert_eq!(
            fs::read(&path).expect("failed to read credential file"),
            br#"{"token":"new"}"#
        );
        let mode = fs::metadata(&path)
            .expect("failed to stat credential file")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o600);

        fs::remove_dir_all(&dir).expect("failed to remove temp dir");
    }

    #[cfg(unix)]
    #[test]
    fn atomic_write_leaves_no_temp_file_after_write_failure() {
        let dir = temp_repo_path("atomic-write-write-failure");
        fs::create_dir_all(&dir).expect("failed to create temp dir");
        let original_permissions = fs::metadata(&dir)
            .expect("failed to stat temp dir")
            .permissions();
        let readonly_permissions = PermissionsExt::from_mode(0o500);
        fs::set_permissions(&dir, readonly_permissions).expect("failed to lock temp dir");

        let target = dir.join("result.json");
        let err = atomic_write(&target, br#"{"ok":true}"#)
            .expect_err("writing inside a non-writable directory should fail");
        assert!(err.to_string().contains("failed to write"));

        let mut entries = fs::read_dir(&dir)
            .expect("failed to read temp dir")
            .map(|entry| {
                entry
                    .expect("failed to read temp dir entry")
                    .file_name()
                    .to_string_lossy()
                    .into_owned()
            })
            .collect::<Vec<_>>();
        entries.sort();
        assert!(entries.is_empty(), "unexpected leftovers: {entries:?}");

        fs::set_permissions(&dir, original_permissions).expect("failed to unlock temp dir");
        fs::remove_dir_all(&dir).expect("failed to remove temp dir");
    }

    #[test]
    fn ensure_repo_layout_collects_all_prune_failures() {
        let repo = temp_repo_path("ensure-repo-layout");
        let mut prune_calls = Vec::new();
        let mut pi_calls = 0usize;

        let err = ensure_repo_layout_with(
            &repo,
            |path, keep| {
                prune_calls.push((path.to_path_buf(), keep));
                match keep {
                    64 => anyhow::bail!("logs failure"),
                    12 if path.ends_with("queue-runs") => anyhow::bail!("queue failure"),
                    _ => Ok(()),
                }
            },
            |_repo_root| {
                pi_calls += 1;
                anyhow::bail!("pi failure")
            },
        )
        .expect_err("prune failures should bubble up after all attempts");

        assert_eq!(pi_calls, 1);
        assert_eq!(prune_calls.len(), 3);
        assert!(
            prune_calls[0].0.ends_with(".auto/logs"),
            "first prune should target logs"
        );
        assert!(
            prune_calls[1].0.ends_with(".auto/fresh-input"),
            "second prune should target fresh-input"
        );
        assert!(
            prune_calls[2].0.ends_with(".auto/queue-runs"),
            "third prune should target queue-runs"
        );
        let message = err.to_string();
        assert!(message.contains(".auto/logs"));
        assert!(message.contains("logs failure"));
        assert!(message.contains(".auto/queue-runs"));
        assert!(message.contains("queue failure"));
        assert!(message.contains(".auto/opencode-data/opencode"));
        assert!(message.contains("pi failure"));

        fs::remove_dir_all(&repo).expect("failed to remove temp repo");
    }
}
