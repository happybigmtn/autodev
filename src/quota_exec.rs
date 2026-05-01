use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use chrono::Utc;

use crate::quota_config::{Provider, QuotaConfig};
use crate::quota_patterns::{self, QuotaVerdict};
use crate::quota_selector;
use crate::quota_state::QuotaState;
use crate::util::write_0o600_if_unix;

/// Guard that restores original auth files on drop.
struct AuthRestoreGuard {
    entries: Vec<AuthBackupEntry>,
    active: bool,
}

struct AuthBackupEntry {
    backup: PathBuf,
    target: PathBuf,
    had_original: bool,
}

impl AuthRestoreGuard {
    fn new(entries: Vec<AuthBackupEntry>) -> Self {
        Self {
            entries,
            active: true,
        }
    }

    fn restore(&mut self) -> Result<()> {
        if !self.active {
            return Ok(());
        }
        restore_auth_backups(&self.entries)?;
        self.active = false;
        Ok(())
    }
}

impl Drop for AuthRestoreGuard {
    fn drop(&mut self) {
        let _ = self.restore();
    }
}

fn restore_auth_backups(entries: &[AuthBackupEntry]) -> Result<()> {
    for entry in entries {
        if entry.backup.exists() {
            if entry.backup.is_dir() {
                remove_and_copy_dir(&entry.backup, &entry.target)?;
            } else {
                copy_file_0o600(&entry.backup, &entry.target)?;
            }
            remove_path(&entry.backup)?;
        } else if !entry.had_original && entry.target.exists() {
            remove_path(&entry.target)?;
        }
    }
    Ok(())
}

fn remove_path(path: &Path) -> Result<()> {
    if path.is_dir() {
        fs::remove_dir_all(path).with_context(|| format!("failed to remove {}", path.display()))
    } else {
        fs::remove_file(path).with_context(|| format!("failed to remove {}", path.display()))
    }
}

fn remove_and_copy_dir(src: &Path, dst: &Path) -> Result<()> {
    if dst.exists() {
        fs::remove_dir_all(dst).with_context(|| format!("failed to remove {}", dst.display()))?;
    }
    copy_dir_recursive(src, dst)
}

fn copy_file_0o600(src: &Path, dst: &Path) -> Result<()> {
    let meta =
        fs::symlink_metadata(src).with_context(|| format!("failed to stat {}", src.display()))?;
    if meta.file_type().is_symlink() {
        bail!(
            "refusing to copy symlinked credential path {}",
            src.display()
        );
    }
    if !meta.is_file() {
        bail!(
            "refusing to copy non-regular credential path {}",
            src.display()
        );
    }
    let bytes = fs::read(src).with_context(|| format!("failed to read {}", src.display()))?;
    if let Some(parent) = dst.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }
    write_0o600_if_unix(dst, &bytes)
}

fn copy_dir_recursive(src: &Path, dst: &Path) -> Result<()> {
    let meta =
        fs::symlink_metadata(src).with_context(|| format!("failed to stat {}", src.display()))?;
    if meta.file_type().is_symlink() {
        bail!(
            "refusing to copy symlinked credential path {}",
            src.display()
        );
    }
    if !meta.is_dir() {
        bail!(
            "refusing to copy non-directory credential path {}",
            src.display()
        );
    }
    fs::create_dir_all(dst).with_context(|| format!("failed to create {}", dst.display()))?;
    for entry in fs::read_dir(src).with_context(|| format!("failed to read {}", src.display()))? {
        let entry = entry?;
        let src_path = entry.path();
        let dst_path = dst.join(entry.file_name());
        let meta = fs::symlink_metadata(&src_path)
            .with_context(|| format!("failed to stat {}", src_path.display()))?;
        if meta.file_type().is_symlink() {
            bail!(
                "refusing to copy symlinked credential path {}",
                src_path.display()
            );
        } else if meta.is_dir() {
            copy_dir_recursive(&src_path, &dst_path)?;
        } else if meta.is_file() {
            copy_file_0o600(&src_path, &dst_path).with_context(|| {
                format!(
                    "failed to copy {} -> {}",
                    src_path.display(),
                    dst_path.display()
                )
            })?;
        } else {
            bail!(
                "refusing to copy non-regular credential path {}",
                src_path.display()
            );
        }
    }
    Ok(())
}

fn copy_profile_to_active_auth(provider: Provider, profile_dir: &Path) -> Result<()> {
    let target = provider.auth_source();

    match provider {
        Provider::Codex => {
            let profile_auth = profile_dir.join("auth.json");
            copy_file_0o600(&profile_auth, &target).with_context(|| {
                format!(
                    "failed to swap credentials from {} to {}",
                    profile_auth.display(),
                    target.display()
                )
            })?;
        }
        Provider::Claude => {
            let home = dirs::home_dir().expect("cannot resolve home directory");
            let claude_json = home.join(".claude.json");

            for entry in fs::read_dir(profile_dir)
                .with_context(|| format!("failed to read profile {}", profile_dir.display()))?
            {
                let entry = entry?;
                let name = entry.file_name();
                let src = entry.path();

                if name == ".claude.json" {
                    copy_file_0o600(&src, &claude_json).with_context(|| {
                        format!(
                            "failed to swap {} -> {}",
                            src.display(),
                            claude_json.display()
                        )
                    })?;
                    continue;
                }

                let meta = fs::symlink_metadata(&src)
                    .with_context(|| format!("failed to stat {}", src.display()))?;
                if meta.file_type().is_symlink() {
                    bail!(
                        "refusing to copy symlinked credential path {}",
                        src.display()
                    );
                }

                let dst = target.join(&name);
                if meta.is_dir() {
                    remove_and_copy_dir(&src, &dst)?;
                } else if meta.is_file() {
                    copy_file_0o600(&src, &dst).with_context(|| {
                        format!("failed to copy {} -> {}", src.display(), dst.display())
                    })?;
                } else {
                    bail!(
                        "refusing to copy non-regular credential path {}",
                        src.display()
                    );
                }
            }
        }
    }

    Ok(())
}

fn swap_credentials(provider: Provider, profile_dir: &Path) -> Result<AuthRestoreGuard> {
    let target = provider.auth_source();
    let backup_dir = QuotaConfig::config_dir().join("backup");
    fs::create_dir_all(&backup_dir).context("failed to create backup directory")?;

    let entries = match provider {
        Provider::Codex => {
            let bp = backup_dir.join("codex-auth.json");
            let had_original = target.exists();
            if had_original {
                copy_file_0o600(&target, &bp)
                    .with_context(|| format!("failed to backup {}", target.display()))?;
            }
            vec![AuthBackupEntry {
                backup: bp,
                target,
                had_original,
            }]
        }
        Provider::Claude => {
            let bp = backup_dir.join("claude");
            let had_original = target.exists();
            if had_original {
                let _ = remove_path(&bp);
                copy_dir_recursive(&target, &bp)
                    .with_context(|| format!("failed to backup {}", target.display()))?;
            }

            let claude_json_bp = backup_dir.join("claude.json");
            let home = dirs::home_dir().expect("cannot resolve home directory");
            let claude_json = home.join(".claude.json");

            // Backup ~/.claude.json separately (lives in home, not in ~/.claude)
            let had_claude_json = claude_json.exists();
            if had_claude_json {
                copy_file_0o600(&claude_json, &claude_json_bp)
                    .with_context(|| format!("failed to backup {}", claude_json.display()))?;
            }

            vec![
                AuthBackupEntry {
                    backup: bp,
                    target,
                    had_original,
                },
                AuthBackupEntry {
                    backup: claude_json_bp,
                    target: claude_json,
                    had_original: had_claude_json,
                },
            ]
        }
    };

    let guard = AuthRestoreGuard::new(entries);
    copy_profile_to_active_auth(provider, profile_dir)?;
    Ok(guard)
}

fn acquire_provider_lock(provider: Provider) -> Result<fd_lock::RwLock<fs::File>> {
    let lock_path = QuotaConfig::config_dir().join(format!("swap-{}.lock", provider.label()));
    fs::create_dir_all(QuotaConfig::config_dir()).context("failed to create quota config dir")?;

    let file = fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(&lock_path)
        .with_context(|| format!("failed to open lock file {}", lock_path.display()))?;

    Ok(fd_lock::RwLock::new(file))
}

#[derive(Debug)]
pub(crate) struct QuotaExecResult {
    pub(crate) exit_status: std::process::ExitStatus,
    pub(crate) stderr_text: String,
}

fn reserve_account_and_swap<'a>(
    provider: Provider,
    config: &'a QuotaConfig,
    scored: &[(
        &'a crate::quota_config::AccountEntry,
        Option<crate::quota_usage::AccountUsage>,
    )],
) -> Result<(String, AuthRestoreGuard)> {
    let mut lock = acquire_provider_lock(provider)?;
    let _write = lock.write().map_err(|e| {
        anyhow::anyhow!("failed to acquire {provider} lock for credential swap: {e}")
    })?;

    let mut state = QuotaState::load()?;
    state.refresh_cooldowns(Utc::now());

    let selected = quota_selector::select_account_from_scores(config, &state, provider, scored)?;
    let account_name = selected.entry.name.clone();
    let profile_dir = QuotaConfig::profile_dir(provider, &account_name)?;

    if !profile_dir.exists() {
        anyhow::bail!(
            "profile directory for account '{account_name}' not found at {}. \
             Run `auto quota accounts capture {account_name}` to fix.",
            profile_dir.display()
        );
    }

    state.mark_selected(&account_name, Utc::now())?;
    state.save()?;

    match swap_credentials(provider, &profile_dir) {
        Ok(guard) => Ok((account_name, guard)),
        Err(error) => {
            state.release_lease(&account_name)?;
            state.save()?;
            Err(error)
        }
    }
}

fn restore_and_update_state(
    provider: Provider,
    account_name: &str,
    restore_guard: &mut AuthRestoreGuard,
    update_state: impl FnOnce(&mut QuotaState, chrono::DateTime<Utc>) -> Result<()>,
) -> Result<()> {
    let mut lock = acquire_provider_lock(provider)?;
    let _write = lock.write().map_err(|e| {
        anyhow::anyhow!("failed to acquire {provider} lock for credential restore: {e}")
    })?;

    let restore_result = restore_guard.restore();

    let now = Utc::now();
    let state_result = (|| -> Result<()> {
        let mut state = QuotaState::load()?;
        state.refresh_cooldowns(now);
        state.release_lease(account_name)?;
        update_state(&mut state, now)?;
        state.save()
    })();

    restore_result?;
    state_result
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
    let max_attempts = config.accounts_for_provider(provider).len();

    for attempt in 0..max_attempts {
        let scored = quota_selector::score_accounts(&config, provider).await?;
        let (account_name, mut guard) = reserve_account_and_swap(provider, &config, &scored)?;

        eprintln!(
            "[quota-router] attempt {}/{max_attempts}: using account '{account_name}'",
            attempt + 1,
        );

        let result = exec_fn().await;

        match result {
            Ok((status, stderr_text)) => {
                let verdict = quota_patterns::check_stderr(provider, &stderr_text);
                restore_and_update_state(provider, &account_name, &mut guard, |state, now| {
                    state.mark_used(&account_name, now)?;
                    match verdict {
                        QuotaVerdict::Exhausted | QuotaVerdict::Unavailable => {
                            state.mark_exhausted(&account_name, now)?;
                        }
                        QuotaVerdict::Ok | QuotaVerdict::OtherError => {
                            if status.success() {
                                state.mark_success(&account_name, now)?;
                            }
                        }
                    }
                    Ok(())
                })?;

                match verdict {
                    QuotaVerdict::Exhausted => {
                        if quota_output_has_agent_progress(&stderr_text) {
                            let recovery_marker =
                                write_quota_progress_recovery_marker(provider, &account_name)?;
                            anyhow::bail!(
                                "account '{account_name}' quota exhausted after worker progress was detected; credentials restored and retry stopped to avoid duplicate side effects. recovery marker: {}",
                                recovery_marker.display()
                            );
                        }
                        eprintln!(
                            "[quota-router] account '{account_name}' quota exhausted, trying next..."
                        );
                        continue;
                    }
                    QuotaVerdict::Unavailable => {
                        eprintln!(
                            "[quota-router] account '{account_name}' auth/availability failed, \
                             trying next..."
                        );
                        continue;
                    }
                    QuotaVerdict::Ok | QuotaVerdict::OtherError => {}
                }

                return Ok(QuotaExecResult {
                    exit_status: status,
                    stderr_text,
                });
            }
            Err(e) => {
                restore_and_update_state(provider, &account_name, &mut guard, |_state, _now| {
                    Ok(())
                })?;
                return Err(e);
            }
        }
    }

    anyhow::bail!(
        "all {provider} accounts exhausted after {max_attempts} attempts. \
         Run `auto quota reset` to force-clear."
    );
}

fn quota_output_has_agent_progress(output: &str) -> bool {
    let lower = output.to_ascii_lowercase();
    lower.contains("agent-progress-detected=true")
        || lower.contains("tokens used")
        || lower.contains("\nexec\n")
        || lower.contains("\napply_patch")
        || lower.contains("patch applied")
        || lower.contains("files changed")
}

#[cfg(test)]
fn restore_credentials(provider: Provider) -> Result<()> {
    let backup_dir = QuotaConfig::config_dir().join("backup");
    let target = provider.auth_source();
    match provider {
        Provider::Codex => {
            let bp = backup_dir.join("codex-auth.json");
            if bp.exists() {
                copy_file_0o600(&bp, &target)?;
                fs::remove_file(&bp)?;
            }
        }
        Provider::Claude => {
            let bp = backup_dir.join("claude");
            if bp.exists() {
                remove_and_copy_dir(&bp, &target)?;
                fs::remove_dir_all(&bp)?;
            }
            let claude_json_bp = backup_dir.join("claude.json");
            let home = dirs::home_dir().expect("cannot resolve home directory");
            let claude_json = home.join(".claude.json");
            if claude_json_bp.exists() {
                copy_file_0o600(&claude_json_bp, &claude_json)?;
                fs::remove_file(&claude_json_bp)?;
            }
        }
    }
    Ok(())
}

fn write_quota_progress_recovery_marker(provider: Provider, account_name: &str) -> Result<PathBuf> {
    let dir = QuotaConfig::config_dir().join("quota-recovery");
    fs::create_dir_all(&dir).with_context(|| format!("failed to create {}", dir.display()))?;
    let path = dir.join(format!(
        "{}-{}-{}.json",
        provider.label(),
        account_name,
        Utc::now().format("%Y%m%d%H%M%S")
    ));
    let body = serde_json::json!({
        "provider": provider.label(),
        "account": account_name,
        "reason": "quota exhausted after worker progress",
        "action": "stopped failover to avoid duplicate side effects",
        "created_at": Utc::now().to_rfc3339(),
    });
    write_0o600_if_unix(&path, serde_json::to_string_pretty(&body)?.as_bytes())?;
    Ok(path)
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
    let scored = quota_selector::score_accounts(&config, provider).await?;
    let (account_name, mut restore_guard) = reserve_account_and_swap(provider, &config, &scored)?;

    eprintln!("[quota-router] selected account '{account_name}'");

    let bin = provider.label();
    let status = std::process::Command::new(bin)
        .args(args)
        .status()
        .with_context(|| format!("failed to launch {bin}"))?;

    restore_and_update_state(provider, &account_name, &mut restore_guard, |state, now| {
        state.mark_used(&account_name, now)?;
        if status.success() {
            state.mark_success(&account_name, now)?;
        }
        Ok(())
    })?;

    Ok(status.code().unwrap_or(1))
}

pub(crate) async fn run_quota_select(provider: Provider) -> Result<()> {
    let mut config = QuotaConfig::load()?;
    let accounts = config.accounts_for_provider(provider);
    if accounts.is_empty() {
        anyhow::bail!(
            "no {provider} accounts configured. \
             Run `auto quota accounts add` to set one up."
        );
    }

    let selected_name = if accounts.len() == 1 {
        accounts[0].name.clone()
    } else {
        eprintln!("Select the primary {provider} account:");
        for (idx, account) in accounts.iter().enumerate() {
            let marker = if config.selected_account_name(provider) == Some(account.name.as_str()) {
                " (current)"
            } else {
                ""
            };
            eprintln!("  {}. {}{}", idx + 1, account.name, marker);
        }
        eprint!("Enter selection [1-{}]: ", accounts.len());
        io::stderr().flush()?;

        let mut input = String::new();
        io::stdin().read_line(&mut input)?;
        let choice = input
            .trim()
            .parse::<usize>()
            .ok()
            .filter(|choice| (1..=accounts.len()).contains(choice))
            .ok_or_else(|| anyhow::anyhow!("invalid selection"))?;
        accounts[choice - 1].name.clone()
    };

    let profile_dir = QuotaConfig::profile_dir(provider, &selected_name)?;

    if !profile_dir.exists() {
        anyhow::bail!(
            "profile directory for account '{selected_name}' not found at {}. \
             Run `auto quota accounts capture {selected_name}` to fix.",
            profile_dir.display()
        );
    }

    config.set_selected_account(provider, &selected_name)?;
    config.save()?;

    let mut lock = acquire_provider_lock(provider)?;
    let _lock_guard = lock.write().map_err(|e| {
        anyhow::anyhow!("failed to acquire {provider} lock for credential swap: {e}")
    })?;
    copy_profile_to_active_auth(provider, &profile_dir)?;

    let mut state = QuotaState::load()?;
    state.refresh_cooldowns(Utc::now());
    state.reset_account(&selected_name)?;
    state.mark_used(&selected_name, Utc::now())?;
    state.save()?;

    eprintln!(
        "[quota-router] primary {provider} account set to '{selected_name}'; active account is '{selected_name}'"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{
        quota_output_has_agent_progress, restore_credentials, run_with_quota, swap_credentials,
    };
    use crate::quota_config::{AccountEntry, Provider, QuotaConfig};

    #[cfg(unix)]
    use std::ffi::OsString;
    #[cfg(unix)]
    use std::fs;
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;
    #[cfg(unix)]
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;
    #[cfg(unix)]
    use std::sync::MutexGuard;
    #[cfg(unix)]
    use std::time::{SystemTime, UNIX_EPOCH};

    #[cfg(unix)]
    struct TempQuotaHome {
        root: PathBuf,
        home_previous: Option<OsString>,
        config_previous: Option<OsString>,
        _lock: MutexGuard<'static, ()>,
        skip_usage_previous: Option<OsString>,
    }

    #[cfg(unix)]
    impl TempQuotaHome {
        fn new(label: &str) -> Self {
            let lock = crate::util::test_process_env_lock()
                .lock()
                .expect("failed to lock process env");
            let stamp = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("system time should be after unix epoch")
                .as_nanos();
            let root = std::env::temp_dir()
                .join(format!("autodev-{label}-{}-{stamp}", std::process::id()));
            let home = root.join("home");
            let config = root.join("config");
            fs::create_dir_all(&home).expect("failed to create temp home");
            fs::create_dir_all(&config).expect("failed to create temp config");
            let home_previous = std::env::var_os("HOME");
            let config_previous = std::env::var_os("XDG_CONFIG_HOME");
            let skip_usage_previous = std::env::var_os("AUTO_QUOTA_SKIP_USAGE");
            std::env::set_var("HOME", &home);
            std::env::set_var("XDG_CONFIG_HOME", &config);
            std::env::set_var("AUTO_QUOTA_SKIP_USAGE", "1");
            Self {
                root,
                home_previous,
                config_previous,
                skip_usage_previous,
                _lock: lock,
            }
        }

        fn home(&self) -> PathBuf {
            self.root.join("home")
        }

        fn profile_dir(&self, provider: Provider, name: &str) -> PathBuf {
            self.root
                .join("config")
                .join("quota-router")
                .join("profiles")
                .join(format!("{}-{name}", provider.label()))
        }

        fn backup_dir(&self) -> PathBuf {
            self.root.join("config").join("quota-router").join("backup")
        }

        fn write_codex_account(&self, name: &str) {
            let profile_dir = self.profile_dir(Provider::Codex, name);
            fs::create_dir_all(&profile_dir).expect("failed to create profile dir");
            fs::write(profile_dir.join("auth.json"), br#"{"tokens":{"access_token":"invalid","refresh_token":"invalid","account_id":"acct"}}"#)
                .expect("failed to write profile auth");
        }
    }

    #[cfg(unix)]
    impl Drop for TempQuotaHome {
        fn drop(&mut self) {
            if let Some(previous) = &self.home_previous {
                std::env::set_var("HOME", previous);
            } else {
                std::env::remove_var("HOME");
            }
            if let Some(previous) = &self.config_previous {
                std::env::set_var("XDG_CONFIG_HOME", previous);
            } else {
                std::env::remove_var("XDG_CONFIG_HOME");
            }
            if let Some(previous) = &self.skip_usage_previous {
                std::env::set_var("AUTO_QUOTA_SKIP_USAGE", previous);
            } else {
                std::env::remove_var("AUTO_QUOTA_SKIP_USAGE");
            }
            let _ = fs::remove_dir_all(&self.root);
        }
    }

    #[cfg(unix)]
    fn set_mode(path: &std::path::Path, mode: u32) {
        let mut permissions = fs::metadata(path)
            .expect("failed to stat file")
            .permissions();
        permissions.set_mode(mode);
        fs::set_permissions(path, permissions).expect("failed to set file permissions");
    }

    #[test]
    fn detects_progress_sentinel_before_quota_failure() {
        assert!(quota_output_has_agent_progress(
            "[auto-loop] agent-progress-detected=true\nError: rate limit exceeded"
        ));
    }

    #[test]
    fn immediate_quota_error_is_not_progress() {
        assert!(!quota_output_has_agent_progress(
            "Error: rate limit exceeded for this organization"
        ));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn quota_exhaustion_after_progress_does_not_try_next_account() {
        let home = TempQuotaHome::new("quota-exec-progress-stop");
        home.write_codex_account("primary");
        home.write_codex_account("secondary");
        fs::create_dir_all(home.home().join(".codex")).expect("failed to create active codex dir");

        let mut config = QuotaConfig::default();
        config
            .add_account(AccountEntry {
                name: "primary".to_string(),
                provider: Provider::Codex,
            })
            .expect("failed to add primary");
        config
            .add_account(AccountEntry {
                name: "secondary".to_string(),
                provider: Provider::Codex,
            })
            .expect("failed to add secondary");
        config.save().expect("failed to save quota config");

        let calls = Arc::new(AtomicUsize::new(0));
        let calls_for_exec = Arc::clone(&calls);
        let error = run_with_quota(Provider::Codex, move || {
            let calls = Arc::clone(&calls_for_exec);
            async move {
                calls.fetch_add(1, Ordering::SeqCst);
                let status = std::process::Command::new("true")
                    .status()
                    .expect("failed to run true");
                Ok((
                    status,
                    "agent-progress-detected=true\nError: rate limit exceeded".to_string(),
                ))
            }
        })
        .await
        .expect_err("progress after quota exhaustion should stop failover");

        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert!(error.to_string().contains("retry stopped"));
        assert!(home
            .root
            .join("config")
            .join("quota-router")
            .join("quota-recovery")
            .exists());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn immediate_quota_error_can_try_next_account() {
        let home = TempQuotaHome::new("quota-exec-immediate-failover");
        home.write_codex_account("primary");
        home.write_codex_account("secondary");
        fs::create_dir_all(home.home().join(".codex")).expect("failed to create active codex dir");

        let mut config = QuotaConfig::default();
        config
            .add_account(AccountEntry {
                name: "primary".to_string(),
                provider: Provider::Codex,
            })
            .expect("failed to add primary");
        config
            .add_account(AccountEntry {
                name: "secondary".to_string(),
                provider: Provider::Codex,
            })
            .expect("failed to add secondary");
        config.save().expect("failed to save quota config");

        let calls = Arc::new(AtomicUsize::new(0));
        let calls_for_exec = Arc::clone(&calls);
        let result = run_with_quota(Provider::Codex, move || {
            let calls = Arc::clone(&calls_for_exec);
            async move {
                let call = calls.fetch_add(1, Ordering::SeqCst);
                let status = std::process::Command::new("true")
                    .status()
                    .expect("failed to run true");
                let stderr = if call == 0 {
                    "Error: rate limit exceeded".to_string()
                } else {
                    String::new()
                };
                Ok((status, stderr))
            }
        })
        .await
        .expect("immediate quota exhaustion should fail over");

        assert_eq!(calls.load(Ordering::SeqCst), 2);
        assert!(result.exit_status.success());
        assert!(result.stderr_text.is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn swap_credentials_enforces_0o600() {
        let home = TempQuotaHome::new("quota-exec-swap");
        let active_dir = home.home().join(".codex");
        fs::create_dir_all(&active_dir).expect("failed to create active auth dir");
        let active_auth = active_dir.join("auth.json");
        fs::write(&active_auth, br#"{"account":"active"}"#).expect("failed to write active auth");
        set_mode(&active_auth, 0o644);

        let profile_dir = home.profile_dir(Provider::Codex, "work");
        fs::create_dir_all(&profile_dir).expect("failed to create profile dir");
        let profile_auth = profile_dir.join("auth.json");
        fs::write(&profile_auth, br#"{"account":"profile"}"#)
            .expect("failed to write profile auth");
        set_mode(&profile_auth, 0o644);

        let guard = swap_credentials(Provider::Codex, &profile_dir)
            .expect("credential swap should succeed");

        let backup_auth = home
            .root
            .join("config")
            .join("quota-router")
            .join("backup")
            .join("codex-auth.json");
        let backup_mode = fs::metadata(&backup_auth)
            .expect("failed to stat credential backup")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(backup_mode, 0o600);

        let mode = fs::metadata(&active_auth)
            .expect("failed to stat swapped auth")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o600);

        drop(guard);

        let restored = fs::read(&active_auth).expect("failed to read restored auth");
        assert_eq!(restored, br#"{"account":"active"}"#);
        let restored_mode = fs::metadata(&active_auth)
            .expect("failed to stat restored auth")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(restored_mode, 0o600);
    }

    #[cfg(unix)]
    #[test]
    fn restore_credentials_restores_claude_json_backup() {
        let home = TempQuotaHome::new("quota-exec-restore-claude-json");
        let active_claude = home.home().join(".claude");
        fs::create_dir_all(&active_claude).expect("failed to create active claude dir");
        fs::write(
            active_claude.join("credentials.json"),
            br#"{"account":"swapped-dir"}"#,
        )
        .expect("failed to write swapped claude credentials");
        fs::write(
            home.home().join(".claude.json"),
            br#"{"account":"swapped-json"}"#,
        )
        .expect("failed to write swapped claude json");

        let backup_dir = home.backup_dir();
        let backup_claude = backup_dir.join("claude");
        fs::create_dir_all(&backup_claude).expect("failed to create claude backup dir");
        fs::write(
            backup_claude.join("credentials.json"),
            br#"{"account":"original-dir"}"#,
        )
        .expect("failed to write claude backup credentials");
        fs::write(
            backup_dir.join("claude.json"),
            br#"{"account":"original-json"}"#,
        )
        .expect("failed to write claude json backup");

        restore_credentials(Provider::Claude).expect("restore should succeed");

        let restored_dir = fs::read(active_claude.join("credentials.json"))
            .expect("failed to read restored claude credentials");
        assert_eq!(restored_dir, br#"{"account":"original-dir"}"#);
        let restored_json = fs::read(home.home().join(".claude.json"))
            .expect("failed to read restored claude json");
        assert_eq!(restored_json, br#"{"account":"original-json"}"#);
        assert!(!backup_claude.exists());
        assert!(!backup_dir.join("claude.json").exists());
    }

    #[cfg(unix)]
    #[test]
    fn swap_credentials_restores_claude_json_on_drop() {
        let home = TempQuotaHome::new("quota-exec-drop-claude-json");
        let active_claude = home.home().join(".claude");
        fs::create_dir_all(&active_claude).expect("failed to create active claude dir");
        fs::write(
            active_claude.join("credentials.json"),
            br#"{"account":"original-dir"}"#,
        )
        .expect("failed to write active claude credentials");
        fs::write(
            home.home().join(".claude.json"),
            br#"{"account":"original-json"}"#,
        )
        .expect("failed to write active claude json");

        let profile_dir = home.profile_dir(Provider::Claude, "work");
        fs::create_dir_all(&profile_dir).expect("failed to create profile dir");
        fs::write(
            profile_dir.join("credentials.json"),
            br#"{"account":"profile-dir"}"#,
        )
        .expect("failed to write profile claude credentials");
        fs::write(
            profile_dir.join(".claude.json"),
            br#"{"account":"profile-json"}"#,
        )
        .expect("failed to write profile claude json");

        let guard = swap_credentials(Provider::Claude, &profile_dir)
            .expect("credential swap should succeed");

        let swapped_json =
            fs::read(home.home().join(".claude.json")).expect("failed to read swapped json");
        assert_eq!(swapped_json, br#"{"account":"profile-json"}"#);

        drop(guard);

        let restored_dir = fs::read(active_claude.join("credentials.json"))
            .expect("failed to read restored claude credentials");
        assert_eq!(restored_dir, br#"{"account":"original-dir"}"#);
        let restored_json = fs::read(home.home().join(".claude.json"))
            .expect("failed to read restored claude json");
        assert_eq!(restored_json, br#"{"account":"original-json"}"#);
    }

    #[cfg(unix)]
    #[test]
    fn swap_credentials_rejects_symlinked_claude_profile_paths() {
        let home = TempQuotaHome::new("quota-exec-symlink-claude");
        let active_claude = home.home().join(".claude");
        fs::create_dir_all(&active_claude).expect("failed to create active claude dir");
        fs::write(
            active_claude.join("credentials.json"),
            br#"{"account":"original-dir"}"#,
        )
        .expect("failed to write active claude credentials");

        let profile_dir = home.profile_dir(Provider::Claude, "work");
        fs::create_dir_all(&profile_dir).expect("failed to create profile dir");
        let real_profile = profile_dir.join("real-credentials.json");
        fs::write(&real_profile, br#"{"account":"profile"}"#)
            .expect("failed to write real profile credentials");
        std::os::unix::fs::symlink(&real_profile, profile_dir.join("credentials.json"))
            .expect("failed to create profile symlink");

        let error = match swap_credentials(Provider::Claude, &profile_dir) {
            Ok(_) => panic!("symlinked claude profile path should be rejected"),
            Err(error) => error.to_string(),
        };

        assert!(error.contains("symlinked credential path"));
        let active = fs::read(active_claude.join("credentials.json"))
            .expect("failed to read restored active claude credentials");
        assert_eq!(active, br#"{"account":"original-dir"}"#);
    }

    #[cfg(unix)]
    #[test]
    fn swap_credentials_removes_codex_auth_when_no_original_existed() {
        let home = TempQuotaHome::new("quota-exec-no-original-codex");
        let active_auth = home.home().join(".codex").join("auth.json");

        let profile_dir = home.profile_dir(Provider::Codex, "work");
        fs::create_dir_all(&profile_dir).expect("failed to create profile dir");
        fs::write(profile_dir.join("auth.json"), br#"{"account":"profile"}"#)
            .expect("failed to write profile auth");

        let guard = swap_credentials(Provider::Codex, &profile_dir)
            .expect("credential swap should succeed");
        assert!(active_auth.exists());

        drop(guard);

        assert!(!active_auth.exists());
    }
}
