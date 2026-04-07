use anyhow::{bail, Result};
use chrono::Utc;

use crate::quota_config::{AccountEntry, Provider, QuotaConfig};
use crate::quota_state::QuotaState;
use crate::quota_usage::{self, AccountUsage};

/// Accounts with less than this weekly remaining are in the "low" tier.
const WEEKLY_FLOOR_PCT: u32 = 15;

#[derive(Debug)]
pub(crate) struct SelectedAccount<'a> {
    pub(crate) entry: &'a AccountEntry,
}

/// Select the best account for the given provider.
///
/// Strategy:
/// 1. Among non-exhausted accounts with ≥15% weekly quota remaining,
///    pick the one whose weekly quota resets soonest.
/// 2. If all accounts have <15% weekly remaining, pick the one with
///    the highest weekly remaining percentage.
/// 3. Accounts whose usage could not be fetched are used only as a
///    last resort.
pub(crate) async fn select_account<'a>(
    config: &'a QuotaConfig,
    state: &QuotaState,
    provider: Provider,
) -> Result<SelectedAccount<'a>> {
    let candidates = config.accounts_for_provider(provider);
    if candidates.is_empty() {
        bail!(
            "no {provider} accounts configured. \
             Run `auto quota accounts add` to set one up."
        );
    }

    let available: Vec<&AccountEntry> = candidates
        .into_iter()
        .filter(|a| !state.get(&a.name).exhausted)
        .collect();

    if available.is_empty() {
        return Err(all_exhausted_error(config, state, provider));
    }

    let mut scored: Vec<(&AccountEntry, Option<AccountUsage>)> =
        Vec::with_capacity(available.len());
    for entry in &available {
        let profile_dir = QuotaConfig::profile_dir(provider, &entry.name);
        match quota_usage::fetch_usage(provider, &profile_dir).await {
            Ok(usage) => scored.push((entry, Some(usage))),
            Err(e) => {
                eprintln!(
                    "[quota-router] failed to fetch usage for '{}': {e:#}",
                    entry.name,
                );
                scored.push((entry, None));
            }
        }
    }

    let selected = pick_best(&scored, state);
    log_selection(selected.entry, &scored);
    Ok(selected)
}

/// Pure scoring logic, separated for testability.
fn pick_best<'a>(
    scored: &[(&'a AccountEntry, Option<AccountUsage>)],
    state: &QuotaState,
) -> SelectedAccount<'a> {
    let above_floor: Vec<_> = scored
        .iter()
        .filter(|(_, u)| {
            u.as_ref()
                .is_some_and(|u| u.weekly_remaining_pct >= WEEKLY_FLOOR_PCT)
        })
        .collect();

    if !above_floor.is_empty() {
        // Soonest weekly reset wins; tiebreak by LRU then name
        let (entry, _) = above_floor
            .iter()
            .min_by(|a, b| {
                let ra = a.1.as_ref().map_or(u64::MAX, |u| u.weekly_resets_in_secs);
                let rb = b.1.as_ref().map_or(u64::MAX, |u| u.weekly_resets_in_secs);
                ra.cmp(&rb)
                    .then_with(|| {
                        state
                            .get(&a.0.name)
                            .last_used
                            .cmp(&state.get(&b.0.name).last_used)
                    })
                    .then_with(|| a.0.name.cmp(&b.0.name))
            })
            .unwrap();
        return SelectedAccount { entry };
    }

    // All below floor (or no usage data): pick highest weekly remaining
    let (entry, _) = scored
        .iter()
        .max_by(|a, b| {
            let ra = a.1.as_ref().map_or(0, |u| u.weekly_remaining_pct);
            let rb = b.1.as_ref().map_or(0, |u| u.weekly_remaining_pct);
            ra.cmp(&rb)
                .then_with(|| {
                    // Reversed: prefer least-recently-used
                    let la = state.get(&a.0.name).last_used;
                    let lb = state.get(&b.0.name).last_used;
                    lb.cmp(&la)
                })
                .then_with(|| b.0.name.cmp(&a.0.name))
        })
        .unwrap();

    SelectedAccount { entry }
}

fn log_selection(
    chosen: &AccountEntry,
    scored: &[(&AccountEntry, Option<AccountUsage>)],
) {
    for (entry, usage) in scored {
        let marker = if entry.name == chosen.name {
            " ← selected"
        } else {
            ""
        };
        match usage {
            Some(u) => eprintln!(
                "[quota-router]   {} session={:>3}% weekly={:>3}% resets_in={}s{marker}",
                entry.name, u.session_used_pct, u.weekly_used_pct, u.session_resets_in_secs,
            ),
            None => eprintln!(
                "[quota-router]   {} (no usage data){marker}",
                entry.name,
            ),
        }
    }
}

fn all_exhausted_error(
    config: &QuotaConfig,
    state: &QuotaState,
    provider: Provider,
) -> anyhow::Error {
    let all_accounts = config.accounts_for_provider(provider);
    let soonest_recovery = all_accounts
        .iter()
        .filter_map(|a| state.get(&a.name).exhausted_at)
        .min();

    if let Some(earliest) = soonest_recovery {
        let recovery = earliest + chrono::Duration::hours(1);
        let wait = recovery.signed_duration_since(Utc::now());
        let minutes = wait.num_minutes().max(0);
        anyhow::anyhow!(
            "all {provider} accounts exhausted. Earliest recovery in ~{minutes}m. \
             Run `auto quota reset` to force-clear."
        )
    } else {
        anyhow::anyhow!(
            "all {provider} accounts exhausted. \
             Run `auto quota reset` to force-clear."
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_account(name: &str, provider: Provider) -> AccountEntry {
        AccountEntry {
            name: name.into(),
            provider,
        }
    }

    fn make_usage(
        session_used_pct: u32,
        session_resets_in_secs: u64,
        weekly_used_pct: u32,
        weekly_resets_in_secs: u64,
    ) -> AccountUsage {
        AccountUsage {
            plan: "test".into(),
            session_used_pct,
            session_resets_in_secs,
            weekly_used_pct,
            weekly_remaining_pct: 100u32.saturating_sub(weekly_used_pct),
            weekly_resets_in_secs,
            limit_reached: false,
        }
    }

    #[test]
    fn soonest_weekly_reset_wins_above_floor() {
        let a = make_account("fast-reset", Provider::Claude);
        let b = make_account("slow-reset", Provider::Claude);
        let state = QuotaState::default();

        // fast-reset: weekly resets in 3600s (1h)
        // slow-reset: weekly resets in 86400s (24h)
        // Both above 15% weekly floor
        let scored: Vec<(&AccountEntry, Option<AccountUsage>)> = vec![
            (&a, Some(make_usage(60, 600, 50, 3600))),
            (&b, Some(make_usage(30, 3600, 50, 86400))),
        ];

        let selected = pick_best(&scored, &state);
        assert_eq!(selected.entry.name, "fast-reset");
    }

    #[test]
    fn highest_weekly_remaining_when_all_below_floor() {
        let a = make_account("low-a", Provider::Claude);
        let b = make_account("low-b", Provider::Claude);
        let state = QuotaState::default();

        // Both below 15% weekly; low-b has more remaining
        let scored: Vec<(&AccountEntry, Option<AccountUsage>)> = vec![
            (&a, Some(make_usage(50, 600, 92, 0))),
            (&b, Some(make_usage(50, 3600, 88, 0))),
        ];

        let selected = pick_best(&scored, &state);
        assert_eq!(selected.entry.name, "low-b"); // 12% > 8%
    }

    #[test]
    fn above_floor_beats_below_floor() {
        let a = make_account("healthy", Provider::Claude);
        let b = make_account("depleted", Provider::Claude);
        let state = QuotaState::default();

        // healthy: 20% weekly remaining (above 15%), resets in 3600s
        // depleted: 5% weekly remaining (below 15%), resets in 100s
        let scored: Vec<(&AccountEntry, Option<AccountUsage>)> = vec![
            (&a, Some(make_usage(50, 3600, 80, 86400))),
            (&b, Some(make_usage(50, 100, 95, 3600))),
        ];

        let selected = pick_best(&scored, &state);
        assert_eq!(selected.entry.name, "healthy");
    }

    #[test]
    fn no_usage_data_is_last_resort() {
        let a = make_account("known", Provider::Claude);
        let b = make_account("unknown", Provider::Claude);
        let state = QuotaState::default();

        let scored: Vec<(&AccountEntry, Option<AccountUsage>)> = vec![
            (&a, Some(make_usage(90, 100, 50, 86400))),
            (&b, None),
        ];

        let selected = pick_best(&scored, &state);
        assert_eq!(selected.entry.name, "known");
    }

    #[test]
    fn no_usage_data_used_when_only_option() {
        let a = make_account("mystery", Provider::Claude);
        let state = QuotaState::default();

        let scored: Vec<(&AccountEntry, Option<AccountUsage>)> = vec![(&a, None)];

        let selected = pick_best(&scored, &state);
        assert_eq!(selected.entry.name, "mystery");
    }

    #[test]
    fn tiebreak_by_lru_then_name() {
        let a = make_account("alpha", Provider::Claude);
        let b = make_account("beta", Provider::Claude);
        let mut state = QuotaState::default();

        let t1 = chrono::DateTime::parse_from_rfc3339("2026-04-07T10:00:00Z")
            .unwrap()
            .to_utc();
        let t2 = chrono::DateTime::parse_from_rfc3339("2026-04-07T11:00:00Z")
            .unwrap()
            .to_utc();
        state.mark_used("alpha", t2); // more recent
        state.mark_used("beta", t1); // less recent

        // Same weekly reset time, both above floor
        let scored: Vec<(&AccountEntry, Option<AccountUsage>)> = vec![
            (&a, Some(make_usage(50, 1000, 50, 86400))),
            (&b, Some(make_usage(50, 1000, 50, 86400))),
        ];

        let selected = pick_best(&scored, &state);
        assert_eq!(selected.entry.name, "beta"); // LRU wins
    }

    #[test]
    fn floor_boundary_exact_15_is_above() {
        let a = make_account("edge", Provider::Claude);
        let state = QuotaState::default();

        // Exactly 15% weekly remaining = above floor
        let scored: Vec<(&AccountEntry, Option<AccountUsage>)> =
            vec![(&a, Some(make_usage(50, 1000, 85, 86400)))];

        let selected = pick_best(&scored, &state);
        assert_eq!(selected.entry.name, "edge");
    }
}
