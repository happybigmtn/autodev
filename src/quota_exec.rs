use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use chrono::Utc;

use crate::quota_config::{Provider, QuotaConfig};
use crate::quota_patterns::{self, QuotaVerdict};
use crate::quota_selector;
use crate::quota_state::QuotaState;

/// Guard that restores original auth files on drop.
struct AuthRestoreGuard {
    pairs: Vec<(PathBuf, PathBuf)>,
    active: bool,
}

impl AuthRestoreGuard {
    fn new(pairs: Vec<(PathBuf, PathBuf)>) -> Self {
        Self {
            pairs,
            active: true,
        }
    }

    fn disarm(&mut self) {
        self.active = false;
    }
}

impl Drop for AuthRestoreGuard {
    fn drop(&mut self) {
        if !self.active {
            return;
        }
        for (backup, target) in &self.pairs {
            if backup.exists() {
                if backup.is_dir() {
                    let _ = remove_and_copy_dir(backup, target);
                } else {
                    let _ = fs::copy(backup, target);
                }
                let _ = remove_path(backup);
            }
        }
    }
}

fn remove_path(path: &Path) -> Result<()> {
    if path.is_dir() {
        fs::remove_dir_all(path)
            .with_context(|| format!("failed to remove {}", path.display()))
    } else {
        fs::remove_file(path)
            .with_context(|| format!("failed to remove {}", path.display()))
    }
}

fn remove_and_copy_dir(src: &Path, dst: &Path) -> Result<()> {
    if dst.exists() {
        fs::remove_dir_all(dst)
            .with_context(|| format!("failed to remove {}", dst.display()))?;
    }
    copy_dir_recursive(src, dst)
}

fn copy_dir_recursive(src: &Path, dst: &Path) -> Result<()> {
    fs::create_dir_all(dst)
        .with_context(|| format!("failed to create {}", dst.display()))?;
    for entry in fs::read_dir(src)
        .with_context(|| format!("failed to read {}", src.display()))?
    {
        let entry = entry?;
        let src_path = entry.path();
        let dst_path = dst.join(entry.file_name());
        let meta = match fs::symlink_metadata(&src_path) {
            Ok(m) => m,
            Err(_) => continue,
        };
        if meta.file_type().is_symlink() {
            // Preserve symlinks as-is; skip if we can't read the target path
            if let Ok(target) = fs::read_link(&src_path) {
                let _ = std::os::unix::fs::symlink(&target, &dst_path);
            }
        } else if meta.is_dir() {
            copy_dir_recursive(&src_path, &dst_path)?;
        } else {
            fs::copy(&src_path, &dst_path).with_context(|| {
                format!("failed to copy {} -> {}", src_path.display(), dst_path.display())
            })?;
        }
    }
    Ok(())
}

fn swap_credentials(provider: Provider, profile_dir: &Path) -> Result<AuthRestoreGuard> {
    let target = provider.auth_source();
    let backup_dir = QuotaConfig::config_dir().join("backup");
    fs::create_dir_all(&backup_dir)
        .context("failed to create backup directory")?;

    let pairs = match provider {
        Provider::Codex => {
            let bp = backup_dir.join("codex-auth.json");
            if target.exists() {
                fs::copy(&target, &bp).with_context(|| {
                    format!("failed to backup {}", target.display())
                })?;
            }
            let profile_auth = profile_dir.join("auth.json");
            fs::copy(&profile_auth, &target).with_context(|| {
                format!(
                    "failed to swap credentials from {} to {}",
                    profile_auth.display(),
                    target.display()
                )
            })?;
            vec![(bp, target)]
        }
        Provider::Claude => {
            let bp = backup_dir.join("claude");
            if target.exists() {
                let _ = remove_path(&bp);
                copy_dir_recursive(&target, &bp)
                    .with_context(|| format!("failed to backup {}", target.display()))?;
            }

            let home = dirs::home_dir().expect("cannot resolve home directory");
            let claude_json = home.join(".claude.json");
            let claude_json_bp = backup_dir.join("claude.json");

            // Backup ~/.claude.json separately (lives in home, not in ~/.claude)
            if claude_json.exists() {
                fs::copy(&claude_json, &claude_json_bp).with_context(|| {
                    format!("failed to backup {}", claude_json.display())
                })?;
            }

            // Copy profile credentials into ~/.claude, but skip .claude.json
            // (it goes to ~/.claude.json instead)
            for entry in fs::read_dir(profile_dir)
                .with_context(|| format!("failed to read profile {}", profile_dir.display()))?
            {
                let entry = entry?;
                let name = entry.file_name();
                let src = entry.path();

                if name == ".claude.json" {
                    fs::copy(&src, &claude_json).with_context(|| {
                        format!("failed to swap {} -> {}", src.display(), claude_json.display())
                    })?;
                    continue;
                }

                let dst = target.join(&name);
                if src.is_dir() {
                    remove_and_copy_dir(&src, &dst)?;
                } else {
                    fs::copy(&src, &dst).with_context(|| {
                        format!("failed to copy {} -> {}", src.display(), dst.display())
                    })?;
                }
            }
            vec![(bp, target), (claude_json_bp, claude_json)]
        }
    };

    Ok(AuthRestoreGuard::new(pairs))
}

fn acquire_swap_lock() -> Result<fd_lock::RwLock<fs::File>> {
    let lock_path = QuotaConfig::config_dir().join("swap.lock");
    fs::create_dir_all(QuotaConfig::config_dir())
        .context("failed to create quota config dir")?;

    let file = fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(&lock_path)
        .with_context(|| format!("failed to open lock file {}", lock_path.display()))?;

    Ok(fd_lock::RwLock::new(file))
}

pub(crate) struct QuotaExecResult {
    pub(crate) exit_status: std::process::ExitStatus,
    pub(crate) account_name: String,
    pub(crate) stderr_text: String,
}

/// Run a CLI command with quota-aware account selection and failover.
///
/// `exec_fn` is called with no arguments (credential swap happens before).
/// Returns `(ExitStatus, stderr_text)`.
pub(crate) async fn run_with_quota<F, Fut>(
    provider: Provider,
    exec_fn: F,
) -> Result<QuotaExecResult>
where
    F: Fn() -> Fut + Send + Sync,
    Fut: std::future::Future<Output = Result<(std::process::ExitStatus, String)>> + Send,
{
    let config = QuotaConfig::load()?;
    let mut state = QuotaState::load()?;
    state.refresh_cooldowns(Utc::now());

    let max_attempts = config.accounts_for_provider(provider).len();

    for attempt in 0..max_attempts {
        let selected = quota_selector::select_account(&config, &state, provider).await?;
        let account_name = selected.entry.name.clone();
        let profile_dir = QuotaConfig::profile_dir(provider, &account_name);

        if !profile_dir.exists() {
            anyhow::bail!(
                "profile directory for account '{account_name}' not found at {}. \
                 Run `auto quota accounts capture {account_name}` to fix.",
                profile_dir.display()
            );
        }

        eprintln!(
            "[quota-router] attempt {}/{max_attempts}: using account '{account_name}'",
            attempt + 1,
        );

        let mut lock = acquire_swap_lock()?;
        let _lock_guard = lock
            .try_write()
            .map_err(|_| anyhow::anyhow!("another quota-router instance holds the swap lock"))?;
        let mut guard = swap_credentials(provider, &profile_dir)?;

        let result = exec_fn().await;

        // Disarm guard and restore manually so we control the order
        guard.disarm();
        drop(guard);
        restore_credentials(provider)?;

        match result {
            Ok((status, stderr_text)) => {
                let verdict = quota_patterns::check_stderr(provider, &stderr_text);
                state.mark_used(&account_name, Utc::now());

                match verdict {
                    QuotaVerdict::Exhausted => {
                        eprintln!(
                            "[quota-router] account '{account_name}' quota exhausted, \
                             trying next..."
                        );
                        state.mark_exhausted(&account_name, Utc::now());
                        state.save()?;
                        continue;
                    }
                    QuotaVerdict::Ok | QuotaVerdict::OtherError => {
                        if status.success() {
                            state.mark_success(&account_name, Utc::now());
                        }
                        state.save()?;
                        return Ok(QuotaExecResult {
                            exit_status: status,
                            account_name,
                            stderr_text,
                        });
                    }
                }
            }
            Err(e) => {
                state.save()?;
                return Err(e);
            }
        }
    }

    state.save()?;
    anyhow::bail!(
        "all {provider} accounts exhausted after {max_attempts} attempts. \
         Run `auto quota reset` to force-clear."
    );
}

fn restore_credentials(provider: Provider) -> Result<()> {
    let backup_dir = QuotaConfig::config_dir().join("backup");
    let target = provider.auth_source();
    match provider {
        Provider::Codex => {
            let bp = backup_dir.join("codex-auth.json");
            if bp.exists() {
                fs::copy(&bp, &target)?;
                fs::remove_file(&bp)?;
            }
        }
        Provider::Claude => {
            let bp = backup_dir.join("claude");
            if bp.exists() {
                remove_and_copy_dir(&bp, &target)?;
                fs::remove_dir_all(&bp)?;
            }
        }
    }
    Ok(())
}

pub(crate) fn is_quota_available(provider: Provider) -> bool {
    QuotaConfig::load_or_none()
        .ok()
        .flatten()
        .is_some_and(|c| !c.accounts_for_provider(provider).is_empty())
}

/// Select the best account, swap credentials, launch the provider CLI
/// with the given args, wait for exit, and restore credentials.
pub(crate) async fn run_quota_open(provider: Provider, args: &[String]) -> Result<i32> {
    let config = QuotaConfig::load()?;
    let mut state = QuotaState::load()?;
    state.refresh_cooldowns(Utc::now());

    let selected = quota_selector::select_account(&config, &state, provider).await?;
    let account_name = selected.entry.name.clone();
    let profile_dir = QuotaConfig::profile_dir(provider, &account_name);

    if !profile_dir.exists() {
        anyhow::bail!(
            "profile directory for account '{account_name}' not found at {}. \
             Run `auto quota accounts capture {account_name}` to fix.",
            profile_dir.display()
        );
    }

    eprintln!("[quota-router] selected account '{account_name}'");

    let mut lock = acquire_swap_lock()?;
    let _lock_guard = lock
        .try_write()
        .map_err(|_| anyhow::anyhow!("another quota-router instance holds the swap lock"))?;
    let guard = swap_credentials(provider, &profile_dir)?;

    let bin = provider.label();
    let status = std::process::Command::new(bin)
        .args(args)
        .status()
        .with_context(|| format!("failed to launch {bin}"))?;

    drop(guard);

    state.mark_used(&account_name, Utc::now());
    if status.success() {
        state.mark_success(&account_name, Utc::now());
    }
    state.save()?;

    Ok(status.code().unwrap_or(1))
}
