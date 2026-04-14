use anyhow::{bail, Result};
use chrono::Utc;

use crate::quota_config::{AccountEntry, Provider, QuotaConfig};
use crate::quota_state::QuotaState;
use crate::quota_usage::{self, AccountUsage};

/// Accounts with less than this weekly remaining are in the "low" tier.
const WEEKLY_FLOOR_PCT: u32 = 15;
/// Accounts with less than this session remaining are avoided when possible.
const SESSION_FLOOR_PCT: u32 = 25;

#[derive(Debug)]
pub(crate) struct SelectedAccount<'a> {
    pub(crate) entry: &'a AccountEntry,
}

/// Select the best account for the given provider.
///
/// Strategy:
/// 1. Prefer accounts with ≥25% session quota remaining.
/// 2. Among those, prefer accounts with ≥15% weekly quota remaining,
///    pick the one whose weekly quota resets soonest.
/// 3. If all session-healthy accounts have <15% weekly remaining, pick the one with
///    the highest weekly remaining percentage.
/// 4. If every known account is below the session floor, pick the one with
///    the highest session remaining percentage.
/// 5. Accounts whose usage could not be fetched are used only as a
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

    let selected = pick_best(&scored, state, config.selected_account_name(provider));
    log_selection(selected.entry, &scored);
    Ok(selected)
}

/// Pure scoring logic, separated for testability.
fn pick_best<'a>(
    scored: &[(&'a AccountEntry, Option<AccountUsage>)],
    state: &QuotaState,
    preferred_name: Option<&str>,
) -> SelectedAccount<'a> {
    if let Some(preferred_name) = preferred_name {
        if let Some((entry, usage)) = scored
            .iter()
            .find(|(entry, _)| entry.name == preferred_name)
        {
            let preferred_is_healthy = usage
                .as_ref()
                .is_none_or(|u| u.session_remaining_pct >= SESSION_FLOOR_PCT);
            if preferred_is_healthy {
                return SelectedAccount { entry };
            }
        }
    }

    let scored_refs: Vec<_> = if let Some(preferred_name) = preferred_name {
        let non_preferred: Vec<_> = scored
            .iter()
            .filter(|(entry, _)| entry.name != preferred_name)
            .collect();
        if non_preferred.is_empty() {
            scored.iter().collect()
        } else {
            non_preferred
        }
    } else {
        scored.iter().collect()
    };

    let session_healthy: Vec<_> = scored_refs
        .iter()
        .copied()
        .filter(|(_, u)| {
            u.as_ref()
                .is_some_and(|u| u.session_remaining_pct >= SESSION_FLOOR_PCT)
        })
        .collect();

    if !session_healthy.is_empty() {
        return pick_best_by_weekly(&session_healthy, state);
    }

    let known_usage: Vec<_> = scored_refs
        .iter()
        .copied()
        .filter(|(_, u)| u.is_some())
        .collect();
    if !known_usage.is_empty() {
        let (entry, _) = known_usage
            .iter()
            .max_by(|a, b| {
                let sa = a.1.as_ref().map_or(0, |u| u.session_remaining_pct);
                let sb = b.1.as_ref().map_or(0, |u| u.session_remaining_pct);
                let wa = a.1.as_ref().map_or(0, |u| u.weekly_remaining_pct);
                let wb = b.1.as_ref().map_or(0, |u| u.weekly_remaining_pct);
                sa.cmp(&sb)
                    .then_with(|| wa.cmp(&wb))
                    .then_with(|| compare_lru_desc(a.0, b.0, state))
                    .then_with(|| b.0.name.cmp(&a.0.name))
            })
            .unwrap();
        return SelectedAccount { entry };
    }

    let (entry, _) = scored_refs
        .iter()
        .max_by(|a, b| compare_lru_then_name_desc(a.0, b.0, state))
        .unwrap();

    SelectedAccount { entry }
}

fn pick_best_by_weekly<'a>(
    candidates: &[&(&'a AccountEntry, Option<AccountUsage>)],
    state: &QuotaState,
) -> SelectedAccount<'a> {
    let above_weekly_floor: Vec<_> = candidates
        .iter()
        .filter(|(_, u)| {
            u.as_ref()
                .is_some_and(|u| u.weekly_remaining_pct >= WEEKLY_FLOOR_PCT)
        })
        .collect();

    if !above_weekly_floor.is_empty() {
        let (entry, _) = above_weekly_floor
            .iter()
            .min_by(|a, b| {
                let ra = a.1.as_ref().map_or(u64::MAX, |u| u.weekly_resets_in_secs);
                let rb = b.1.as_ref().map_or(u64::MAX, |u| u.weekly_resets_in_secs);
                ra.cmp(&rb)
                    .then_with(|| compare_lru_asc(a.0, b.0, state))
                    .then_with(|| a.0.name.cmp(&b.0.name))
            })
            .unwrap();
        return SelectedAccount { entry };
    }

    let (entry, _) = candidates
        .iter()
        .max_by(|a, b| {
            let ra = a.1.as_ref().map_or(0, |u| u.weekly_remaining_pct);
            let rb = b.1.as_ref().map_or(0, |u| u.weekly_remaining_pct);
            ra.cmp(&rb)
                .then_with(|| compare_lru_desc(a.0, b.0, state))
                .then_with(|| b.0.name.cmp(&a.0.name))
        })
        .unwrap();

    SelectedAccount { entry }
}

fn compare_lru_asc(a: &AccountEntry, b: &AccountEntry, state: &QuotaState) -> std::cmp::Ordering {
    state
        .get(&a.name)
        .last_used
        .cmp(&state.get(&b.name).last_used)
}

fn compare_lru_desc(a: &AccountEntry, b: &AccountEntry, state: &QuotaState) -> std::cmp::Ordering {
    compare_lru_asc(b, a, state)
}

fn compare_lru_then_name_desc(
    a: &AccountEntry,
    b: &AccountEntry,
    state: &QuotaState,
) -> std::cmp::Ordering {
    compare_lru_desc(a, b, state).then_with(|| b.name.cmp(&a.name))
}

fn log_selection(chosen: &AccountEntry, scored: &[(&AccountEntry, Option<AccountUsage>)]) {
    for (entry, usage) in scored {
        let marker = if entry.name == chosen.name {
            " ← selected"
        } else {
            ""
        };
        match usage {
            Some(u) => eprintln!(
                "[quota-router]   {} session_used={:>3}% weekly_remaining={:>3}% weekly_resets_in={}s session_resets_in={}s{marker}",
                entry.name,
                u.session_used_pct,
                u.weekly_remaining_pct,
                u.weekly_resets_in_secs,
                u.session_resets_in_secs,
            ),
            None => eprintln!("[quota-router]   {} (no usage data){marker}", entry.name,),
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
            session_remaining_pct: 100u32.saturating_sub(session_used_pct),
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

        let selected = pick_best(&scored, &state, None);
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

        let selected = pick_best(&scored, &state, None);
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

        let selected = pick_best(&scored, &state, None);
        assert_eq!(selected.entry.name, "healthy");
    }

    #[test]
    fn no_usage_data_is_last_resort() {
        let a = make_account("known", Provider::Claude);
        let b = make_account("unknown", Provider::Claude);
        let state = QuotaState::default();

        let scored: Vec<(&AccountEntry, Option<AccountUsage>)> =
            vec![(&a, Some(make_usage(90, 100, 50, 86400))), (&b, None)];

        let selected = pick_best(&scored, &state, None);
        assert_eq!(selected.entry.name, "known");
    }

    #[test]
    fn no_usage_data_used_when_only_option() {
        let a = make_account("mystery", Provider::Claude);
        let state = QuotaState::default();

        let scored: Vec<(&AccountEntry, Option<AccountUsage>)> = vec![(&a, None)];

        let selected = pick_best(&scored, &state, None);
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

        let selected = pick_best(&scored, &state, None);
        assert_eq!(selected.entry.name, "beta"); // LRU wins
    }

    #[test]
    fn floor_boundary_exact_15_is_above() {
        let a = make_account("edge", Provider::Claude);
        let state = QuotaState::default();

        // Exactly 15% weekly remaining = above floor
        let scored: Vec<(&AccountEntry, Option<AccountUsage>)> =
            vec![(&a, Some(make_usage(50, 1000, 85, 86400)))];

        let selected = pick_best(&scored, &state, None);
        assert_eq!(selected.entry.name, "edge");
    }

    #[test]
    fn session_floor_skips_low_five_hour_candidate() {
        let a = make_account("low-session", Provider::Claude);
        let b = make_account("healthy-session", Provider::Claude);
        let state = QuotaState::default();

        let scored: Vec<(&AccountEntry, Option<AccountUsage>)> = vec![
            (&a, Some(make_usage(80, 600, 10, 3600))), // 20% session remaining
            (&b, Some(make_usage(60, 600, 30, 7200))), // 40% session remaining
        ];

        let selected = pick_best(&scored, &state, None);
        assert_eq!(selected.entry.name, "healthy-session");
    }

    #[test]
    fn highest_session_remaining_wins_when_all_below_session_floor() {
        let a = make_account("almost-spent", Provider::Claude);
        let b = make_account("less-spent", Provider::Claude);
        let state = QuotaState::default();

        let scored: Vec<(&AccountEntry, Option<AccountUsage>)> = vec![
            (&a, Some(make_usage(95, 600, 5, 3600))), // 5% session remaining
            (&b, Some(make_usage(78, 600, 80, 86400))), // 22% session remaining
        ];

        let selected = pick_best(&scored, &state, None);
        assert_eq!(selected.entry.name, "less-spent");
    }

    #[test]
    fn preferred_account_wins_while_session_is_healthy() {
        let a = make_account("preferred", Provider::Claude);
        let b = make_account("other", Provider::Claude);
        let state = QuotaState::default();

        let scored: Vec<(&AccountEntry, Option<AccountUsage>)> = vec![
            (&a, Some(make_usage(50, 600, 95, 3600))),
            (&b, Some(make_usage(40, 600, 50, 7200))),
        ];

        let selected = pick_best(&scored, &state, Some("preferred"));
        assert_eq!(selected.entry.name, "preferred");
    }

    #[test]
    fn preferred_account_is_skipped_below_session_floor() {
        let a = make_account("preferred", Provider::Claude);
        let b = make_account("other", Provider::Claude);
        let state = QuotaState::default();

        let scored: Vec<(&AccountEntry, Option<AccountUsage>)> = vec![
            (&a, Some(make_usage(80, 600, 10, 3600))),
            (&b, Some(make_usage(40, 600, 50, 7200))),
        ];

        let selected = pick_best(&scored, &state, Some("preferred"));
        assert_eq!(selected.entry.name, "other");
    }
}
