use std::collections::HashMap;
use std::fs;

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::quota_config::{validate_account_name, QuotaConfig};
use crate::util::write_0o600_if_unix;

/// How long an exhausted account stays unavailable before auto-retrying.
const EXHAUSTION_COOLDOWN_HOURS: i64 = 1;

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub(crate) struct AccountState {
    pub(crate) exhausted: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) exhausted_at: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub(crate) active_leases: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) last_used: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) last_success: Option<DateTime<Utc>>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub(crate) struct QuotaState {
    pub(crate) accounts: HashMap<String, AccountState>,
}

impl QuotaState {
    pub(crate) fn state_path() -> std::path::PathBuf {
        QuotaConfig::config_dir().join("state.json")
    }

    pub(crate) fn load() -> Result<Self> {
        let path = Self::state_path();
        if !path.exists() {
            return Ok(Self::default());
        }
        let text = fs::read_to_string(&path)
            .with_context(|| format!("failed to read {}", path.display()))?;
        serde_json::from_str(&text).with_context(|| format!("failed to parse {}", path.display()))
    }

    pub(crate) fn save(&self) -> Result<()> {
        let path = Self::state_path();
        let dir = QuotaConfig::config_dir();
        fs::create_dir_all(&dir).with_context(|| format!("failed to create {}", dir.display()))?;
        let text = serde_json::to_string_pretty(self).context("failed to serialize quota state")?;
        write_0o600_if_unix(&path, text.as_bytes())
    }

    pub(crate) fn get(&self, name: &str) -> AccountState {
        self.accounts.get(name).cloned().unwrap_or_default()
    }

    pub(crate) fn mark_exhausted(&mut self, name: &str, now: DateTime<Utc>) -> Result<()> {
        validate_account_name(name)?;
        let state = self.accounts.entry(name.to_owned()).or_default();
        state.exhausted = true;
        state.exhausted_at = Some(now);
        Ok(())
    }

    pub(crate) fn mark_used(&mut self, name: &str, now: DateTime<Utc>) -> Result<()> {
        validate_account_name(name)?;
        let state = self.accounts.entry(name.to_owned()).or_default();
        state.last_used = Some(now);
        Ok(())
    }

    pub(crate) fn mark_selected(&mut self, name: &str, now: DateTime<Utc>) -> Result<()> {
        validate_account_name(name)?;
        let state = self.accounts.entry(name.to_owned()).or_default();
        state.last_used = Some(now);
        state.active_leases = state.active_leases.saturating_add(1);
        Ok(())
    }

    pub(crate) fn mark_success(&mut self, name: &str, now: DateTime<Utc>) -> Result<()> {
        validate_account_name(name)?;
        let state = self.accounts.entry(name.to_owned()).or_default();
        state.exhausted = false;
        state.exhausted_at = None;
        state.last_success = Some(now);
        Ok(())
    }

    pub(crate) fn release_lease(&mut self, name: &str) -> Result<()> {
        validate_account_name(name)?;
        let state = self.accounts.entry(name.to_owned()).or_default();
        state.active_leases = state.active_leases.saturating_sub(1);
        Ok(())
    }

    /// Clear exhausted flag on accounts that have cooled down.
    pub(crate) fn refresh_cooldowns(&mut self, now: DateTime<Utc>) {
        let cooldown = chrono::Duration::hours(EXHAUSTION_COOLDOWN_HOURS);
        for state in self.accounts.values_mut() {
            if state.exhausted {
                if let Some(at) = state.exhausted_at {
                    if now - at >= cooldown {
                        state.exhausted = false;
                        state.exhausted_at = None;
                    }
                }
            }
        }
    }

    /// Manually reset an account's exhausted status.
    pub(crate) fn reset_account(&mut self, name: &str) -> Result<()> {
        validate_account_name(name)?;
        if let Some(state) = self.accounts.get_mut(name) {
            state.exhausted = false;
            state.exhausted_at = None;
            state.active_leases = 0;
        }
        Ok(())
    }

    /// Reset all accounts' exhausted status.
    pub(crate) fn reset_all(&mut self) {
        for state in self.accounts.values_mut() {
            state.exhausted = false;
            state.exhausted_at = None;
            state.active_leases = 0;
        }
    }
}

fn is_zero(value: &u32) -> bool {
    *value == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    use std::ffi::OsString;
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;
    #[cfg(unix)]
    use std::path::PathBuf;
    #[cfg(unix)]
    use std::sync::MutexGuard;
    #[cfg(unix)]
    use std::time::{SystemTime, UNIX_EPOCH};

    #[cfg(unix)]
    struct TempConfigHome {
        root: PathBuf,
        previous: Option<OsString>,
        _lock: MutexGuard<'static, ()>,
    }

    #[cfg(unix)]
    impl TempConfigHome {
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
            fs::create_dir_all(&root).expect("failed to create temp config root");
            let previous = std::env::var_os("XDG_CONFIG_HOME");
            std::env::set_var("XDG_CONFIG_HOME", &root);
            Self {
                root,
                previous,
                _lock: lock,
            }
        }
    }

    #[cfg(unix)]
    impl Drop for TempConfigHome {
        fn drop(&mut self) {
            if let Some(previous) = &self.previous {
                std::env::set_var("XDG_CONFIG_HOME", previous);
            } else {
                std::env::remove_var("XDG_CONFIG_HOME");
            }
            let _ = fs::remove_dir_all(&self.root);
        }
    }

    #[test]
    fn cooldown_clears_after_duration() {
        let exhausted_time = DateTime::parse_from_rfc3339("2026-04-07T10:00:00Z")
            .unwrap()
            .to_utc();
        let after_cooldown = DateTime::parse_from_rfc3339("2026-04-07T11:01:00Z")
            .unwrap()
            .to_utc();

        let mut state = QuotaState::default();
        state.mark_exhausted("test", exhausted_time).unwrap();
        assert!(state.get("test").exhausted);

        state.refresh_cooldowns(after_cooldown);
        assert!(!state.get("test").exhausted);
    }

    #[test]
    fn cooldown_keeps_within_window() {
        let exhausted_time = DateTime::parse_from_rfc3339("2026-04-07T10:00:00Z")
            .unwrap()
            .to_utc();
        let still_cooling = DateTime::parse_from_rfc3339("2026-04-07T10:30:00Z")
            .unwrap()
            .to_utc();

        let mut state = QuotaState::default();
        state.mark_exhausted("test", exhausted_time).unwrap();

        state.refresh_cooldowns(still_cooling);
        assert!(state.get("test").exhausted);
    }

    #[test]
    fn manual_reset_clears_immediately() {
        let now = Utc::now();
        let mut state = QuotaState::default();
        state.mark_exhausted("test", now).unwrap();
        assert!(state.get("test").exhausted);

        state.reset_account("test").unwrap();
        assert!(!state.get("test").exhausted);
    }

    #[test]
    fn reset_all_clears_everything() {
        let now = Utc::now();
        let mut state = QuotaState::default();
        state.mark_exhausted("a", now).unwrap();
        state.mark_exhausted("b", now).unwrap();

        state.reset_all();
        assert!(!state.get("a").exhausted);
        assert!(!state.get("b").exhausted);
    }

    #[test]
    fn mark_success_clears_exhaustion_state() {
        let now = Utc::now();
        let mut state = QuotaState::default();
        state.mark_exhausted("test", now).unwrap();
        assert!(state.get("test").exhausted);

        state.mark_success("test", now).unwrap();

        let account = state.get("test");
        assert!(!account.exhausted);
        assert!(account.exhausted_at.is_none());
        assert_eq!(account.last_success, Some(now));
    }

    #[test]
    fn mark_selected_increments_and_release_lease_decrements() {
        let now = Utc::now();
        let mut state = QuotaState::default();

        state.mark_selected("test", now).unwrap();
        state.mark_selected("test", now).unwrap();
        assert_eq!(state.get("test").active_leases, 2);
        assert_eq!(state.get("test").last_used, Some(now));

        state.release_lease("test").unwrap();
        assert_eq!(state.get("test").active_leases, 1);

        state.release_lease("test").unwrap();
        state.release_lease("test").unwrap();
        assert_eq!(state.get("test").active_leases, 0);
    }

    #[cfg(unix)]
    #[test]
    fn save_writes_owner_only() {
        let _config_home = TempConfigHome::new("quota-state-save");
        let now = Utc::now();
        let mut state = QuotaState::default();
        state.mark_exhausted("test", now).unwrap();

        state.save().expect("state save should succeed");

        let mode = fs::metadata(QuotaState::state_path())
            .expect("failed to stat saved state")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o600);
    }
}
