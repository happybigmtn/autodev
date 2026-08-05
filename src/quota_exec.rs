use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{bail, Context, Result};
use chrono::Utc;

use crate::quota_config::{
    codex_home_for_profile, codex_live_home, codex_profile_uses_isolated_home, Provider,
    QuotaConfig,
};
use crate::quota_patterns::{self, QuotaVerdict};
use crate::quota_selector;
use crate::quota_state::QuotaState;
use crate::util::write_0o600_if_unix;

/// Information about the account the quota router selected for the
/// current invocation. Passed to the `exec_fn` closure so the spawn
/// site can inject per-profile env vars (e.g. `CODEX_HOME`).
#[derive(Clone, Debug)]
pub(crate) struct SelectedAccount {
    pub(crate) name: String,
    pub(crate) provider: Provider,
    pub(crate) profile_dir: PathBuf,
    pub(crate) live: bool,
}

impl SelectedAccount {
    /// True iff this Codex profile uses the isolated `codex-home/`
    /// subdir layout. In that case the router skips the legacy
    /// `~/.codex/auth.json` file swap and instead spawns codex with
    /// `CODEX_HOME=<profile_dir>/codex-home`.
    pub(crate) fn uses_isolated_codex_home(&self) -> bool {
        matches!(self.provider, Provider::Codex)
            && codex_profile_uses_isolated_home(&self.profile_dir)
    }

    /// Env vars to inject into the provider CLI process. Currently
    /// emits `CODEX_HOME` for live Codex accounts and Codex profiles
    /// with an isolated home.
    pub(crate) fn extra_env(&self) -> Vec<(String, String)> {
        if self.live {
            vec![(
                "CODEX_HOME".to_string(),
                codex_live_home().to_string_lossy().into_owned(),
            )]
        } else if self.uses_isolated_codex_home() {
            let home = codex_home_for_profile(&self.profile_dir);
            vec![(
                "CODEX_HOME".to_string(),
                home.to_string_lossy().into_owned(),
            )]
        } else {
            Vec::new()
        }
    }
}

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

fn sync_newer_claude_credentials(profile_dir: &Path, active_dir: &Path) -> Result<()> {
    let profile_creds = profile_dir.join(".credentials.json");
    let active_creds = active_dir.join(".credentials.json");

    let Some(profile_expires_at) = claude_oauth_expires_at(&profile_creds)? else {
        return Ok(());
    };
    let Some(active_expires_at) = claude_oauth_expires_at(&active_creds)? else {
        return Ok(());
    };

    if active_expires_at <= profile_expires_at {
        return Ok(());
    }

    copy_file_0o600(&active_creds, &profile_creds).with_context(|| {
        format!(
            "failed to refresh Claude profile credentials from {} -> {}",
            active_creds.display(),
            profile_creds.display()
        )
    })?;
    eprintln!(
        "[quota-router] synced newer Claude credentials from {} into profile {}",
        active_creds.display(),
        profile_creds.display()
    );
    Ok(())
}

fn claude_oauth_expires_at(path: &Path) -> Result<Option<i64>> {
    let meta = match fs::symlink_metadata(path) {
        Ok(meta) => meta,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(error).with_context(|| format!("failed to stat {}", path.display()))
        }
    };
    if meta.file_type().is_symlink() {
        bail!(
            "refusing to read symlinked credential path {}",
            path.display()
        );
    }
    if !meta.is_file() {
        bail!(
            "refusing to read non-regular credential path {}",
            path.display()
        );
    }

    let text =
        fs::read_to_string(path).with_context(|| format!("failed to read {}", path.display()))?;
    let creds: serde_json::Value = serde_json::from_str(&text)
        .with_context(|| format!("failed to parse {}", path.display()))?;
    Ok(creds["claudeAiOauth"]["expiresAt"].as_i64())
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
            sync_newer_claude_credentials(profile_dir, &target)?;
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

fn swap_credentials(account: &SelectedAccount) -> Result<AuthRestoreGuard> {
    if account.live {
        return Ok(AuthRestoreGuard::new(Vec::new()));
    }

    // Isolated Codex profiles use CODEX_HOME=<profile_dir>/codex-home at
    // spawn time, so we don't need to swap `~/.codex/auth.json` at all.
    // The empty guard makes restore a no-op.
    if account.uses_isolated_codex_home() {
        return Ok(AuthRestoreGuard::new(Vec::new()));
    }

    let provider = account.provider;
    let profile_dir = account.profile_dir.as_path();
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

#[cfg(test)]
fn swap_credentials_legacy(provider: Provider, profile_dir: &Path) -> Result<AuthRestoreGuard> {
    let account = SelectedAccount {
        name: "test".to_string(),
        provider,
        profile_dir: profile_dir.to_path_buf(),
        live: false,
    };
    swap_credentials(&account)
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
) -> Result<(SelectedAccount, AuthRestoreGuard)> {
    let mut lock = acquire_provider_lock(provider)?;
    let _write = lock.write().map_err(|e| {
        anyhow::anyhow!("failed to acquire {provider} lock for credential swap: {e}")
    })?;

    let mut state = QuotaState::load()?;
    state.refresh_cooldowns(Utc::now());

    let selected = quota_selector::select_account_from_scores(config, &state, provider, scored)?;
    let account_name = selected.entry.name.clone();
    let profile_dir = if selected.entry.live {
        codex_live_home()
    } else {
        QuotaConfig::profile_dir(provider, &account_name)?
    };

    if !selected.entry.live && !profile_dir.exists() {
        anyhow::bail!(
            "profile directory for account '{account_name}' not found at {}. \
             Run `auto quota accounts capture {account_name}` to fix.",
            profile_dir.display()
        );
    }

    let account = SelectedAccount {
        name: account_name.clone(),
        provider,
        profile_dir,
        live: selected.entry.live,
    };

    state.mark_selected(&account_name, Utc::now())?;
    state.save()?;

    match swap_credentials(&account) {
        Ok(guard) => Ok((account, guard)),
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

/// Default cap on the cumulative time a single `run_with_quota` call will spend
/// waiting for a Codex/Claude *session* quota window to reset before giving up
/// and surfacing the exhaustion error. A transient session window is ~15 min, so
/// 20 min covers one window plus margin while staying bounded. Override with
/// `AUTO_QUOTA_BACKOFF_MAX_SECS`; set it to `0` to disable backoff entirely
/// (restoring the pre-2026-07 "fail immediately when all accounts are
/// exhausted" behavior).
const DEFAULT_QUOTA_BACKOFF_MAX_SECS: u64 = 1200;

/// Extra seconds slept past a reported reset horizon so the provider-side window
/// has definitely rolled over before we retry.
const QUOTA_BACKOFF_MARGIN_SECS: u64 = 15;

fn quota_backoff_cap() -> Duration {
    let secs = std::env::var("AUTO_QUOTA_BACKOFF_MAX_SECS")
        .ok()
        .and_then(|v| v.trim().parse::<u64>().ok())
        .unwrap_or(DEFAULT_QUOTA_BACKOFF_MAX_SECS);
    Duration::from_secs(secs)
}

/// Decide whether to back off and wait for a session-quota reset after every
/// account was exhausted/unavailable in a single selection pass, and for how
/// long. Pure so the policy is unit-testable without live accounts.
///
/// Returns `Some(sleep)` to wait then retry the whole selection loop, or `None`
/// to give up (surface the exhaustion error). We only wait when ALL of:
/// - backoff is enabled (`cap` > 0),
/// - at least one account hit a genuine *quota* exhaustion this pass (waiting
///   cannot fix a pass that was purely auth/unavailable failures),
/// - a concrete soonest session-reset horizon is known from live usage, and
/// - waiting until that reset (plus a small margin) still fits inside the
///   remaining cap budget — if even the soonest reset is beyond budget, waiting
///   cannot recover in time, so we surface the error immediately rather than
///   sleep pointlessly.
pub(crate) fn quota_backoff_wait(
    cap: Duration,
    already_waited: Duration,
    saw_quota_exhaustion: bool,
    soonest_session_reset_secs: Option<u64>,
) -> Option<Duration> {
    if cap.is_zero() || !saw_quota_exhaustion {
        return None;
    }
    let reset = soonest_session_reset_secs?;
    let target =
        Duration::from_secs(reset).saturating_add(Duration::from_secs(QUOTA_BACKOFF_MARGIN_SECS));
    let remaining = cap.checked_sub(already_waited).filter(|r| !r.is_zero())?;
    if target > remaining {
        return None;
    }
    Some(target)
}

/// Soonest session-reset horizon (seconds) across all accounts that reported
/// live usage this pass — the earliest moment ANY account should be usable
/// again. `None` when no account reported usage data.
fn soonest_session_reset(
    scored: &[(
        &crate::quota_config::AccountEntry,
        Option<crate::quota_usage::AccountUsage>,
    )],
) -> Option<u64> {
    scored
        .iter()
        .filter_map(|(_, usage)| usage.as_ref().map(|u| u.session_resets_in_secs))
        .min()
}

// ── Orchestrator lane-level quota ride-out (F1 single-account gap) ─────────
//
// `run_with_quota`'s backoff only fires when an entire account-selection pass
// is exhausted. It does NOT cover the common single-live-account case: the
// router selects the only account (no alternative), the Codex lane exec runs
// and fails with a usage-limit signature, and — because Codex reports that
// signature on its `--json` stdout in some modes (so `check_stderr` on the
// captured stderr never sees it) — `run_with_quota` returns a non-zero exit
// that the parallel orchestrator treats as a plain task failure (retry ×N then
// shelve, killing the run on a transient window). These helpers let the
// orchestrator recognize that signature, wait out the session reset, and
// re-dispatch the SAME task instead of shelving.

/// Default cap (seconds) for the orchestrator's *lane-level* quota ride-out when
/// `AUTO_QUOTA_BACKOFF_MAX_SECS` is unset. Larger than the in-loop
/// `DEFAULT_QUOTA_BACKOFF_MAX_SECS` (1200) because riding out a session reset
/// *between* lane dispatches holds no worker, lease, or credential guard and
/// strictly prevents a run from dying on a transient window, so tolerating a
/// full ~1h Codex session reset is worth it. Setting `AUTO_QUOTA_BACKOFF_MAX_SECS`
/// overrides this on both paths; `=0` disables the ride-out entirely. The
/// in-loop backoff default is deliberately left unchanged.
const DEFAULT_LANE_QUOTA_BACKOFF_MAX_SECS: u64 = 5400;

/// Hard cap on how many quota ride-out waits a single task may take before it is
/// allowed to shelve normally — a belt-and-suspenders guard against a
/// persistently-exhausted account stalling one task forever (the cumulative
/// time budget is the primary bound; this stops a pathological drip of tiny
/// sub-cap waits from looping).
pub(crate) const LANE_QUOTA_MAX_WAITS_PER_TASK: u32 = 8;

/// Cap for the orchestrator lane-level quota ride-out. Honors
/// `AUTO_QUOTA_BACKOFF_MAX_SECS` when set (single operator dial; `=0` disables
/// ride-out), otherwise uses `DEFAULT_LANE_QUOTA_BACKOFF_MAX_SECS`.
pub(crate) fn lane_quota_backoff_cap() -> Duration {
    match std::env::var("AUTO_QUOTA_BACKOFF_MAX_SECS")
        .ok()
        .and_then(|v| v.trim().parse::<u64>().ok())
    {
        Some(secs) => Duration::from_secs(secs),
        None => Duration::from_secs(DEFAULT_LANE_QUOTA_BACKOFF_MAX_SECS),
    }
}

/// Pure decision for the orchestrator lane-level quota ride-out. Given the
/// per-task wait ledger and the observed exhaustion signals, returns
/// `Some(sleep)` to wait then re-dispatch the SAME task, or `None` to fall
/// through to the normal task-failure retry/shelve path. Reuses
/// `quota_backoff_wait` so the time-budget policy is identical to the in-loop
/// backoff. Returns `None` whenever: the per-task wait count is spent, the
/// failure is not a real quota signal, not every account is session-exhausted,
/// the reset horizon is unknown, or the wait would exceed the remaining cap
/// budget — so an unknown or longer-than-cap reset shelves rather than spins.
pub(crate) fn lane_quota_backoff_decision(
    cap: Duration,
    already_waited: Duration,
    waits_taken: u32,
    signature_exhausted: bool,
    all_accounts_session_exhausted: bool,
    soonest_session_reset_secs: Option<u64>,
) -> Option<Duration> {
    if waits_taken >= LANE_QUOTA_MAX_WAITS_PER_TASK {
        return None;
    }
    let saw_quota_exhaustion = signature_exhausted && all_accounts_session_exhausted;
    quota_backoff_wait(
        cap,
        already_waited,
        saw_quota_exhaustion,
        soonest_session_reset_secs,
    )
}

/// True when an error message (e.g. a `run_with_quota` all-accounts bail) is the
/// quota-exhaustion signal, so the orchestrator can ride it out rather than
/// shelve on it.
pub(crate) fn error_text_is_quota_exhaustion(text: &str) -> bool {
    text.contains("accounts exhausted after")
}

const LANE_LOG_TAIL_BYTES: u64 = 65_536;

fn read_log_tail(path: &Path, max_bytes: u64) -> String {
    use std::io::{Read, Seek, SeekFrom};
    let Ok(mut file) = fs::File::open(path) else {
        return String::new();
    };
    let len = file.metadata().map(|m| m.len()).unwrap_or(0);
    if len > max_bytes {
        if file.seek(SeekFrom::Start(len - max_bytes)).is_err() {
            return String::new();
        }
        let mut buf = Vec::with_capacity(max_bytes as usize);
        if file.take(max_bytes).read_to_end(&mut buf).is_err() {
            return String::new();
        }
        String::from_utf8_lossy(&buf).into_owned()
    } else {
        fs::read_to_string(path).unwrap_or_default()
    }
}

/// Scan the tail of a lane's stdout/stderr logs for a provider quota/usage-limit
/// signature, reusing the same `quota_patterns` machinery as the exec seam.
/// Codex emits the usage-limit line on its `--json` stdout stream in some
/// failure modes and on stderr in others, so both are scanned (stderr first).
pub(crate) fn lane_output_quota_verdict(
    provider: Provider,
    stdout_log: Option<&Path>,
    stderr_log: Option<&Path>,
) -> QuotaVerdict {
    for path in [stderr_log, stdout_log].into_iter().flatten() {
        let tail = read_log_tail(path, LANE_LOG_TAIL_BYTES);
        match quota_patterns::check_stderr(provider, &tail) {
            QuotaVerdict::Exhausted => return QuotaVerdict::Exhausted,
            QuotaVerdict::Unavailable => return QuotaVerdict::Unavailable,
            QuotaVerdict::Ok | QuotaVerdict::OtherError => {}
        }
    }
    QuotaVerdict::OtherError
}

/// Live-usage probe for the orchestrator quota ride-out. Returns
/// `Some((all_accounts_session_exhausted, soonest_session_reset_secs))` for the
/// provider, or `None` when usage cannot be determined (no config, all-invalid
/// credentials, or usage lookups disabled) — in which case the caller must NOT
/// wait, so a missing reset horizon can never cause a hot-loop. `all_exhausted`
/// is false if any account has unknown or non-exhausted usage, so the router
/// could still route around the exhaustion and we should not pause the run.
pub(crate) async fn probe_session_exhaustion(provider: Provider) -> Option<(bool, Option<u64>)> {
    let config = QuotaConfig::load_or_none().ok().flatten()?;
    if config.accounts_for_provider(provider).is_empty() {
        return None;
    }
    let scored = quota_selector::score_accounts(&config, provider)
        .await
        .ok()?;
    let mut any_usage = false;
    let mut all_exhausted = true;
    let mut soonest: Option<u64> = None;
    for (_, usage) in &scored {
        match usage {
            Some(u) => {
                any_usage = true;
                if u.limit_reached || u.session_remaining_pct == 0 {
                    soonest = Some(soonest.map_or(u.session_resets_in_secs, |cur| {
                        cur.min(u.session_resets_in_secs)
                    }));
                } else {
                    all_exhausted = false;
                }
            }
            // Unknown usage (fetch failed, non-auth): treat as possibly-healthy
            // so we don't pause when the router could route around it.
            None => all_exhausted = false,
        }
    }
    if !any_usage {
        return None;
    }
    Some((all_exhausted, soonest))
}

/// Run a CLI command with quota-aware account selection and failover.
///
/// `exec_fn` is invoked with the `SelectedAccount` the router chose; the
/// closure must merge `account.extra_env()` into its own env when spawning
/// the provider CLI so isolated CODEX_HOME profiles route correctly.
/// Returns `(ExitStatus, stderr_text)`.
///
/// When every configured account is session-exhausted in a full pass, this
/// backs off and waits for the soonest session-quota reset (bounded by
/// `AUTO_QUOTA_BACKOFF_MAX_SECS`) and retries, so a transient ~15-min session
/// window can no longer turn a queued run's worth of work into a cascade of
/// shelved tasks. Genuine sustained exhaustion (soonest reset beyond the cap,
/// e.g. a weekly limit) still surfaces the error after the bounded wait.
pub(crate) async fn run_with_quota<F, Fut>(
    provider: Provider,
    exec_fn: F,
) -> Result<QuotaExecResult>
where
    F: Fn(SelectedAccount) -> Fut + Send + Sync,
    Fut: std::future::Future<Output = Result<(std::process::ExitStatus, String)>> + Send,
{
    let config = QuotaConfig::load()?;
    let max_attempts = config.accounts_for_provider(provider).len();
    let backoff_cap = quota_backoff_cap();
    let mut waited = Duration::ZERO;

    loop {
        let mut saw_quota_exhaustion = false;
        let mut soonest_reset: Option<u64> = None;

        for attempt in 0..max_attempts {
            let scored = quota_selector::score_accounts(&config, provider).await?;
            if let Some(reset) = soonest_session_reset(&scored) {
                soonest_reset = Some(soonest_reset.map_or(reset, |cur| cur.min(reset)));
            }
            let (account, mut guard) = reserve_account_and_swap(provider, &config, &scored)?;
            let account_name = account.name.clone();

            eprintln!(
                "[quota-router] attempt {}/{max_attempts}: using account '{account_name}'",
                attempt + 1,
            );

            let result = exec_fn(account.clone()).await;

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
                            saw_quota_exhaustion = true;
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
                    restore_and_update_state(
                        provider,
                        &account_name,
                        &mut guard,
                        |_state, _now| Ok(()),
                    )?;
                    return Err(e);
                }
            }
        }

        // Every account was exhausted/unavailable this pass. If the exhaustion
        // is a transient session window whose reset lands inside our remaining
        // wait budget, sleep for it and retry the whole loop instead of failing
        // the run. No lock/lease/credential guard is held here (each attempt
        // restored its guard before `continue`), so the sleep is safe and does
        // not block other lanes' account selection.
        match quota_backoff_wait(backoff_cap, waited, saw_quota_exhaustion, soonest_reset) {
            Some(sleep_for) => {
                waited = waited.saturating_add(sleep_for);
                eprintln!(
                    "[quota-router] all {provider} accounts session-exhausted; backing off {}s for the soonest session reset, then resuming dispatch (waited {}s / {}s cap)",
                    sleep_for.as_secs(),
                    waited.as_secs(),
                    backoff_cap.as_secs(),
                );
                tokio::time::sleep(sleep_for).await;
                // Loop: score_accounts re-fetches live usage; the reset should
                // now show session headroom and the next exec should succeed.
                continue;
            }
            None => break,
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

/// True when `err` (anywhere in its chain) says every configured account
/// for the provider has dead credentials. Exec seams use this to fall
/// back to the provider's default login instead of failing the whole
/// run — a model phase should not die because the router's account pool
/// went stale while the default `~/.codex` / `claude` login still works.
pub(crate) fn error_is_all_accounts_invalid(err: &anyhow::Error) -> bool {
    err.chain()
        .any(|cause| cause.is::<crate::quota_selector::AllAccountsInvalid>())
}

/// Cached all-invalid errors mean the same safe fallback should happen, but
/// the detailed probe warnings were already emitted on the first live pass.
/// Exec seams use this marker to avoid repeating an unchanged warning cycle.
pub(crate) fn error_is_cached_all_accounts_invalid(err: &anyhow::Error) -> bool {
    err.chain().any(|cause| {
        cause
            .downcast_ref::<crate::quota_selector::AllAccountsInvalid>()
            .is_some_and(|invalid| invalid.cached)
    })
}

/// Select the best account, swap credentials, launch the provider CLI
/// with the given args, wait for exit, and restore credentials.
pub(crate) async fn run_quota_open(provider: Provider, args: &[String]) -> Result<i32> {
    let config = QuotaConfig::load()?;
    let scored = quota_selector::score_accounts(&config, provider).await?;
    let (account, mut restore_guard) = reserve_account_and_swap(provider, &config, &scored)?;
    let account_name = account.name.clone();

    eprintln!("[quota-router] selected account '{account_name}'");

    let bin = provider.label();
    let mut cmd = std::process::Command::new(bin);
    cmd.args(args);
    for (key, value) in account.extra_env() {
        cmd.env(key, value);
    }
    let status = cmd
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
    // Isolated Codex profiles (`<profile_dir>/codex-home/auth.json`)
    // route via CODEX_HOME at spawn time, so they don't need the legacy
    // `~/.codex/auth.json` swap.
    if !(matches!(provider, Provider::Codex) && codex_profile_uses_isolated_home(&profile_dir)) {
        copy_profile_to_active_auth(provider, &profile_dir)?;
    }

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
        claude_oauth_expires_at, error_is_all_accounts_invalid,
        error_is_cached_all_accounts_invalid, quota_backoff_wait, quota_output_has_agent_progress,
        restore_credentials, run_with_quota, swap_credentials_legacy as swap_credentials,
        sync_newer_claude_credentials, Duration,
    };
    use crate::quota_config::{AccountEntry, Provider, QuotaConfig};

    #[test]
    fn cached_all_invalid_error_preserves_fallback_type_and_suppression_marker() {
        let cached = anyhow::Error::new(crate::quota_selector::AllAccountsInvalid {
            provider: Provider::Codex,
            cached: true,
        });
        assert!(error_is_all_accounts_invalid(&cached));
        assert!(error_is_cached_all_accounts_invalid(&cached));

        let fresh = anyhow::Error::new(crate::quota_selector::AllAccountsInvalid {
            provider: Provider::Codex,
            cached: false,
        });
        assert!(error_is_all_accounts_invalid(&fresh));
        assert!(!error_is_cached_all_accounts_invalid(&fresh));
    }

    #[test]
    fn backoff_waits_for_soonest_session_reset_within_cap() {
        // Transient session window: reset in 933s, 20-min cap -> wait reset+margin.
        let wait = quota_backoff_wait(Duration::from_secs(1200), Duration::ZERO, true, Some(933));
        assert_eq!(wait, Some(Duration::from_secs(933 + 15)));
    }

    #[test]
    fn backoff_disabled_when_cap_is_zero() {
        assert_eq!(
            quota_backoff_wait(Duration::ZERO, Duration::ZERO, true, Some(60)),
            None
        );
    }

    #[test]
    fn backoff_skipped_when_no_quota_exhaustion_seen() {
        // A pass that failed purely on auth/unavailable must not wait.
        assert_eq!(
            quota_backoff_wait(Duration::from_secs(1200), Duration::ZERO, false, Some(60)),
            None
        );
    }

    #[test]
    fn backoff_skipped_without_a_known_reset_horizon() {
        assert_eq!(
            quota_backoff_wait(Duration::from_secs(1200), Duration::ZERO, true, None),
            None
        );
    }

    #[test]
    fn backoff_gives_up_when_soonest_reset_exceeds_remaining_budget() {
        // Weekly-scale exhaustion (reset beyond cap): surface the error instead
        // of sleeping pointlessly.
        assert_eq!(
            quota_backoff_wait(
                Duration::from_secs(1200),
                Duration::ZERO,
                true,
                Some(100_000)
            ),
            None
        );
    }

    #[test]
    fn backoff_respects_cumulative_budget_across_passes() {
        // Already waited 1150s of a 1200s cap: only 50s left, a 60s reset+margin
        // won't fit -> give up (bounded total wait).
        assert_eq!(
            quota_backoff_wait(
                Duration::from_secs(1200),
                Duration::from_secs(1150),
                true,
                Some(60)
            ),
            None
        );
        // But a short reset that fits the remaining budget is still honored.
        assert_eq!(
            quota_backoff_wait(
                Duration::from_secs(1200),
                Duration::from_secs(1100),
                true,
                Some(60)
            ),
            Some(Duration::from_secs(75))
        );
    }

    #[test]
    fn backoff_gives_up_once_budget_is_fully_spent() {
        assert_eq!(
            quota_backoff_wait(
                Duration::from_secs(600),
                Duration::from_secs(600),
                true,
                Some(1)
            ),
            None
        );
    }

    // ── Orchestrator lane-level quota ride-out ────────────────────────────

    use super::{
        error_text_is_quota_exhaustion, lane_output_quota_verdict, lane_quota_backoff_decision,
        LANE_QUOTA_MAX_WAITS_PER_TASK,
    };
    use crate::quota_patterns::QuotaVerdict;

    #[test]
    fn lane_ride_out_waits_then_retries_on_near_reset() {
        // (a) usage-limit lane failure with a known near reset within cap and
        // every account session-exhausted -> wait reset+margin, then re-dispatch.
        let wait = lane_quota_backoff_decision(
            Duration::from_secs(5400),
            Duration::ZERO,
            0,
            /* signature_exhausted */ true,
            /* all_accounts_session_exhausted */ true,
            Some(3600),
        );
        assert_eq!(wait, Some(Duration::from_secs(3600 + 15)));
    }

    #[test]
    fn lane_ride_out_falls_through_when_reset_unknown_or_beyond_cap() {
        // (b1) reset unknown -> None (shelve, never spin on a missing horizon).
        assert_eq!(
            lane_quota_backoff_decision(
                Duration::from_secs(5400),
                Duration::ZERO,
                0,
                true,
                true,
                None,
            ),
            None
        );
        // (b2) reset beyond cap (weekly-scale) -> None (shelve after no wait).
        assert_eq!(
            lane_quota_backoff_decision(
                Duration::from_secs(5400),
                Duration::ZERO,
                0,
                true,
                true,
                Some(100_000),
            ),
            None
        );
    }

    #[test]
    fn lane_ride_out_ignores_genuine_non_quota_failure() {
        // (c) a real (non-quota) task failure: no usage-limit signature -> None,
        // so it is NEVER mistaken for quota and rides the normal retry/shelve.
        assert_eq!(
            lane_quota_backoff_decision(
                Duration::from_secs(5400),
                Duration::ZERO,
                0,
                /* signature_exhausted */ false,
                true,
                Some(60),
            ),
            None
        );
        // A quota-looking signature but the router still has a non-exhausted /
        // unknown account -> don't pause the run (let the router route around it).
        assert_eq!(
            lane_quota_backoff_decision(
                Duration::from_secs(5400),
                Duration::ZERO,
                0,
                true,
                /* all_accounts_session_exhausted */ false,
                Some(60),
            ),
            None
        );
    }

    #[test]
    fn lane_ride_out_is_bounded_by_max_waits_per_task() {
        // Even with a fits-in-budget reset, once the per-task wait count is spent
        // the task must be allowed to shelve rather than loop forever.
        assert_eq!(
            lane_quota_backoff_decision(
                Duration::from_secs(100_000),
                Duration::ZERO,
                LANE_QUOTA_MAX_WAITS_PER_TASK,
                true,
                true,
                Some(60),
            ),
            None
        );
        // One below the cap still rides out.
        assert_eq!(
            lane_quota_backoff_decision(
                Duration::from_secs(100_000),
                Duration::ZERO,
                LANE_QUOTA_MAX_WAITS_PER_TASK - 1,
                true,
                true,
                Some(60),
            ),
            Some(Duration::from_secs(75))
        );
    }

    #[test]
    fn lane_ride_out_disabled_when_cap_is_zero() {
        // AUTO_QUOTA_BACKOFF_MAX_SECS=0 disables ride-out (backward compatible).
        assert_eq!(
            lane_quota_backoff_decision(Duration::ZERO, Duration::ZERO, 0, true, true, Some(60)),
            None
        );
    }

    #[test]
    fn error_text_quota_exhaustion_matches_bail() {
        assert!(error_text_is_quota_exhaustion(
            "all Codex accounts exhausted after 2 attempts. Run `auto quota reset` to force-clear."
        ));
        assert!(!error_text_is_quota_exhaustion(
            "failed to launch Codex at /usr/bin/codex: No such file or directory"
        ));
    }

    #[test]
    fn lane_output_verdict_detects_usage_limit_on_stdout() {
        // Codex writes the usage-limit line on its --json stdout in the failure
        // mode this fix targets; the stderr log is empty. The tail scan must
        // still classify it as Exhausted.
        let dir = TempDir::new();
        let stdout_log = dir.path().join("stdout.log");
        let stderr_log = dir.path().join("stderr.log");
        fs::write(
            &stdout_log,
            b"{\"type\":\"error\",\"message\":\"You've hit your usage limit. Try again at 2:31 PM.\"}\n",
        )
        .expect("write stdout log");
        fs::write(&stderr_log, b"").expect("write stderr log");

        assert_eq!(
            lane_output_quota_verdict(Provider::Codex, Some(&stdout_log), Some(&stderr_log),),
            QuotaVerdict::Exhausted
        );
    }

    #[test]
    fn lane_output_verdict_ignores_ordinary_failure_logs() {
        let dir = TempDir::new();
        let stderr_log = dir.path().join("stderr.log");
        fs::write(&stderr_log, b"error[E0308]: mismatched types\n").expect("write stderr log");
        assert_eq!(
            lane_output_quota_verdict(Provider::Codex, None, Some(&stderr_log)),
            QuotaVerdict::OtherError
        );
    }

    #[cfg(unix)]
    use std::ffi::OsString;
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

    struct TempDir {
        path: std::path::PathBuf,
    }

    impl TempDir {
        fn new() -> Self {
            let unique = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock should be after unix epoch")
                .as_nanos();
            let path = std::env::temp_dir().join(format!("autodev-quota-exec-{unique}"));
            fs::create_dir_all(&path).expect("temp dir should be created");
            Self { path }
        }

        fn path(&self) -> &std::path::Path {
            &self.path
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.path);
        }
    }

    fn write_claude_creds(dir: &std::path::Path, expires_at: i64, refresh_token: &str) {
        let path = dir.join(".credentials.json");
        let body = serde_json::json!({
            "claudeAiOauth": {
                "accessToken": "access",
                "refreshToken": refresh_token,
                "expiresAt": expires_at,
                "scopes": ["user:profile"],
            }
        });
        fs::write(
            path,
            serde_json::to_vec(&body).expect("json should serialize"),
        )
        .expect("creds should write");
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
                live: false,
            })
            .expect("failed to add primary");
        config
            .add_account(AccountEntry {
                name: "secondary".to_string(),
                provider: Provider::Codex,
                live: false,
            })
            .expect("failed to add secondary");
        config.save().expect("failed to save quota config");

        let calls = Arc::new(AtomicUsize::new(0));
        let calls_for_exec = Arc::clone(&calls);
        let error = run_with_quota(Provider::Codex, move |_account| {
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
                live: false,
            })
            .expect("failed to add primary");
        config
            .add_account(AccountEntry {
                name: "secondary".to_string(),
                provider: Provider::Codex,
                live: false,
            })
            .expect("failed to add secondary");
        config.save().expect("failed to save quota config");

        let calls = Arc::new(AtomicUsize::new(0));
        let calls_for_exec = Arc::clone(&calls);
        let result = run_with_quota(Provider::Codex, move |_account| {
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

    #[cfg(unix)]
    #[test]
    fn isolated_codex_home_skips_active_auth_swap() {
        use crate::quota_exec::SelectedAccount;
        let home = TempQuotaHome::new("quota-exec-isolated-codex");
        let active_auth = home.home().join(".codex").join("auth.json");
        fs::create_dir_all(active_auth.parent().unwrap())
            .expect("failed to create active codex dir");
        fs::write(&active_auth, br#"{"account":"untouched"}"#).expect("failed to seed active auth");

        let profile_dir = home.profile_dir(Provider::Codex, "isolated");
        let codex_home = profile_dir.join("codex-home");
        fs::create_dir_all(&codex_home).expect("failed to create codex-home subdir");
        fs::write(codex_home.join("auth.json"), br#"{"account":"isolated"}"#)
            .expect("failed to write isolated auth");
        fs::write(codex_home.join("installation_id"), b"isolated-uuid\n")
            .expect("failed to write isolated installation_id");

        let account = SelectedAccount {
            name: "isolated".to_string(),
            provider: Provider::Codex,
            profile_dir: profile_dir.clone(),
            live: false,
        };
        assert!(account.uses_isolated_codex_home());
        let env = account.extra_env();
        assert_eq!(env.len(), 1);
        assert_eq!(env[0].0, "CODEX_HOME");
        assert_eq!(PathBuf::from(&env[0].1), codex_home);

        let guard = super::swap_credentials(&account).expect("isolated swap should succeed");
        // Active auth was NOT replaced — the worker is expected to read from
        // CODEX_HOME=<profile_dir>/codex-home instead.
        let active = fs::read(&active_auth).expect("active auth should remain");
        assert_eq!(active, br#"{"account":"untouched"}"#);
        drop(guard);
        let active_after = fs::read(&active_auth).expect("active auth should still be present");
        assert_eq!(active_after, br#"{"account":"untouched"}"#);
    }

    #[cfg(unix)]
    #[test]
    fn live_codex_home_skips_active_auth_swap() {
        use crate::quota_config::codex_live_home;
        use crate::quota_exec::SelectedAccount;

        let home = TempQuotaHome::new("quota-exec-live-codex");
        let live_home = codex_live_home();
        let active_auth = live_home.join("auth.json");
        fs::create_dir_all(&live_home).expect("failed to create live codex dir");
        fs::write(&active_auth, br#"{"account":"untouched"}"#).expect("failed to seed active auth");

        let profile_dir = home.profile_dir(Provider::Codex, "live");
        fs::create_dir_all(&profile_dir).expect("failed to create profile dir");
        fs::write(profile_dir.join("auth.json"), br#"{"account":"profile"}"#)
            .expect("failed to seed profile auth");

        let account = SelectedAccount {
            name: "live".to_string(),
            provider: Provider::Codex,
            profile_dir,
            live: true,
        };
        let env = account.extra_env();
        assert_eq!(env.len(), 1);
        assert_eq!(env[0].0, "CODEX_HOME");
        assert_eq!(PathBuf::from(&env[0].1), live_home);

        let guard = super::swap_credentials(&account).expect("live swap should be a no-op");
        let active = fs::read(&active_auth).expect("active auth should remain");
        assert_eq!(active, br#"{"account":"untouched"}"#);
        drop(guard);
        let active_after = fs::read(&active_auth).expect("active auth should still be present");
        assert_eq!(active_after, br#"{"account":"untouched"}"#);
    }

    #[test]
    fn sync_newer_claude_credentials_updates_stale_profile() {
        let profile = TempDir::new();
        let active = TempDir::new();
        write_claude_creds(profile.path(), 100, "old-refresh");
        write_claude_creds(active.path(), 200, "new-refresh");

        sync_newer_claude_credentials(profile.path(), active.path()).expect("sync should succeed");

        let profile_expires_at = claude_oauth_expires_at(&profile.path().join(".credentials.json"))
            .expect("read should succeed");
        assert_eq!(profile_expires_at, Some(200));

        let synced: serde_json::Value = serde_json::from_slice(
            &fs::read(profile.path().join(".credentials.json")).expect("profile creds should read"),
        )
        .expect("profile creds should parse");
        assert_eq!(
            synced["claudeAiOauth"]["refreshToken"].as_str(),
            Some("new-refresh")
        );
    }

    #[test]
    fn sync_newer_claude_credentials_keeps_newer_profile() {
        let profile = TempDir::new();
        let active = TempDir::new();
        write_claude_creds(profile.path(), 300, "profile-refresh");
        write_claude_creds(active.path(), 200, "active-refresh");

        sync_newer_claude_credentials(profile.path(), active.path()).expect("sync should succeed");

        let synced: serde_json::Value = serde_json::from_slice(
            &fs::read(profile.path().join(".credentials.json")).expect("profile creds should read"),
        )
        .expect("profile creds should parse");
        assert_eq!(
            synced["claudeAiOauth"]["refreshToken"].as_str(),
            Some("profile-refresh")
        );
    }

    #[cfg(unix)]
    #[test]
    fn sync_newer_claude_credentials_rejects_symlinked_profile_credentials() {
        let profile = TempDir::new();
        let active = TempDir::new();
        let real_profile_creds = profile.path().join("real-credentials.json");
        write_claude_creds(profile.path(), 100, "old-refresh");
        fs::rename(
            profile.path().join(".credentials.json"),
            &real_profile_creds,
        )
        .expect("failed to move profile credentials");
        std::os::unix::fs::symlink(
            &real_profile_creds,
            profile.path().join(".credentials.json"),
        )
        .expect("failed to create profile credential symlink");
        write_claude_creds(active.path(), 200, "new-refresh");

        let error = sync_newer_claude_credentials(profile.path(), active.path())
            .expect_err("symlinked profile credentials should be rejected");

        assert!(error.to_string().contains("symlinked credential path"));
        let profile_expires_at =
            claude_oauth_expires_at(&real_profile_creds).expect("read should succeed");
        assert_eq!(profile_expires_at, Some(100));
    }

    #[cfg(unix)]
    #[test]
    fn sync_newer_claude_credentials_rejects_symlinked_active_credentials() {
        let profile = TempDir::new();
        let active = TempDir::new();
        let real_active_creds = active.path().join("real-credentials.json");
        write_claude_creds(profile.path(), 100, "old-refresh");
        write_claude_creds(active.path(), 200, "new-refresh");
        fs::rename(active.path().join(".credentials.json"), &real_active_creds)
            .expect("failed to move active credentials");
        std::os::unix::fs::symlink(&real_active_creds, active.path().join(".credentials.json"))
            .expect("failed to create active credential symlink");

        let error = sync_newer_claude_credentials(profile.path(), active.path())
            .expect_err("symlinked active credentials should be rejected");

        assert!(error.to_string().contains("symlinked credential path"));
        let synced: serde_json::Value = serde_json::from_slice(
            &fs::read(profile.path().join(".credentials.json")).expect("profile creds should read"),
        )
        .expect("profile creds should parse");
        assert_eq!(
            synced["claudeAiOauth"]["refreshToken"].as_str(),
            Some("old-refresh")
        );
    }

    #[cfg(unix)]
    #[test]
    fn sync_newer_claude_credentials_preserves_owner_only_mode() {
        let profile = TempDir::new();
        let active = TempDir::new();
        let profile_creds = profile.path().join(".credentials.json");
        write_claude_creds(profile.path(), 100, "old-refresh");
        write_claude_creds(active.path(), 200, "new-refresh");
        set_mode(&profile_creds, 0o644);

        sync_newer_claude_credentials(profile.path(), active.path()).expect("sync should succeed");

        let mode = fs::metadata(&profile_creds)
            .expect("failed to stat refreshed profile credentials")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o600);
    }
}
