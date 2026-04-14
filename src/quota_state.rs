use std::collections::HashMap;
use std::fs;

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::quota_config::QuotaConfig;

/// How long an exhausted account stays unavailable before auto-retrying.
const EXHAUSTION_COOLDOWN_HOURS: i64 = 1;

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub(crate) struct AccountState {
    pub(crate) exhausted: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) exhausted_at: Option<DateTime<Utc>>,
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
        fs::write(&path, text.as_bytes())
            .with_context(|| format!("failed to write {}", path.display()))
    }

    pub(crate) fn get(&self, name: &str) -> AccountState {
        self.accounts.get(name).cloned().unwrap_or_default()
    }

    pub(crate) fn mark_exhausted(&mut self, name: &str, now: DateTime<Utc>) {
        let state = self.accounts.entry(name.to_owned()).or_default();
        state.exhausted = true;
        state.exhausted_at = Some(now);
    }

    pub(crate) fn mark_used(&mut self, name: &str, now: DateTime<Utc>) {
        let state = self.accounts.entry(name.to_owned()).or_default();
        state.last_used = Some(now);
    }

    pub(crate) fn mark_success(&mut self, name: &str, now: DateTime<Utc>) {
        let state = self.accounts.entry(name.to_owned()).or_default();
        state.last_success = Some(now);
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
    pub(crate) fn reset_account(&mut self, name: &str) {
        if let Some(state) = self.accounts.get_mut(name) {
            state.exhausted = false;
            state.exhausted_at = None;
        }
    }

    /// Reset all accounts' exhausted status.
    pub(crate) fn reset_all(&mut self) {
        for state in self.accounts.values_mut() {
            state.exhausted = false;
            state.exhausted_at = None;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cooldown_clears_after_duration() {
        let exhausted_time = DateTime::parse_from_rfc3339("2026-04-07T10:00:00Z")
            .unwrap()
            .to_utc();
        let after_cooldown = DateTime::parse_from_rfc3339("2026-04-07T11:01:00Z")
            .unwrap()
            .to_utc();

        let mut state = QuotaState::default();
        state.mark_exhausted("test", exhausted_time);
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
        state.mark_exhausted("test", exhausted_time);

        state.refresh_cooldowns(still_cooling);
        assert!(state.get("test").exhausted);
    }

    #[test]
    fn manual_reset_clears_immediately() {
        let now = Utc::now();
        let mut state = QuotaState::default();
        state.mark_exhausted("test", now);
        assert!(state.get("test").exhausted);

        state.reset_account("test");
        assert!(!state.get("test").exhausted);
    }

    #[test]
    fn reset_all_clears_everything() {
        let now = Utc::now();
        let mut state = QuotaState::default();
        state.mark_exhausted("a", now);
        state.mark_exhausted("b", now);

        state.reset_all();
        assert!(!state.get("a").exhausted);
        assert!(!state.get("b").exhausted);
    }
}
