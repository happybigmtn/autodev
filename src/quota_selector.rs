use anyhow::{bail, Result};

use crate::quota_config::{AccountEntry, Provider, QuotaConfig};
use crate::quota_state::QuotaState;
use crate::quota_usage::{self, AccountUsage};

/// Accounts with less than this weekly remaining are in the "low" tier.
const WEEKLY_FLOOR_PCT: u32 = 10;
/// Accounts with less than this session remaining are avoided when possible.
const SESSION_FLOOR_PCT: u32 = 25;

#[derive(Debug)]
pub(crate) struct SelectedAccount<'a> {
    pub(crate) entry: &'a AccountEntry,
}

pub(crate) async fn score_accounts(
    config: &QuotaConfig,
    provider: Provider,
) -> Result<Vec<(&AccountEntry, Option<AccountUsage>)>> {
    let candidates = config.accounts_for_provider(provider);
    if candidates.is_empty() {
        bail!(
            "no {provider} accounts configured. \
             Run `auto quota accounts add` to set one up."
        );
    }

    let mut scored: Vec<(&AccountEntry, Option<AccountUsage>)> =
        Vec::with_capacity(candidates.len());
    for entry in candidates {
        let profile_dir = QuotaConfig::profile_dir(provider, &entry.name);
        match quota_usage::fetch_usage(provider, &profile_dir).await {
            Ok(usage) => scored.push((entry, Some(usage))),
            Err(e) => {
                eprintln!(
                    "[quota-router] failed to fetch usage for '{}': {}",
                    entry.name,
                    quota_usage::sanitize_quota_error_message(&e),
                );
                scored.push((entry, None));
            }
        }
    }

    Ok(scored)
}

/// Select the best account from pre-fetched quota scores.
///
/// Strategy:
/// 1. Exclude known accounts with <10% weekly quota remaining.
/// 2. Prefer accounts with ≥25% session quota remaining.
/// 3. Among those, pick the one whose weekly quota resets soonest.
/// 4. If every known account is below the session floor, pick the one with
///    the highest session remaining percentage.
/// 5. Accounts whose usage could not be fetched are used only as a
///    last resort.
pub(crate) fn select_account_from_scores<'a>(
    config: &'a QuotaConfig,
    state: &QuotaState,
    provider: Provider,
    scored: &[(&'a AccountEntry, Option<AccountUsage>)],
) -> Result<SelectedAccount<'a>> {
    if scored.is_empty() {
        bail!(
            "no {provider} accounts configured. \
             Run `auto quota accounts add` to set one up."
        );
    }

    let available = selectable_scored_candidates(scored, state);
    let available = if available.is_empty() {
        eprintln!(
            "[quota-router] every {provider} account is marked exhausted in local state; rechecking live usage before refusing to run"
        );
        scored.to_vec()
    } else {
        available
    };

    let below_weekly_floor = low_weekly_account_summaries(&available);
    if !below_weekly_floor.is_empty() {
        eprintln!(
            "[quota-router] skipping accounts below {WEEKLY_FLOOR_PCT}% weekly quota: {}",
            below_weekly_floor.join(", ")
        );
    }

    let weekly_eligible = weekly_floor_candidates(&available);
    if weekly_eligible.is_empty() {
        bail!(
            "no selectable {provider} account has at least {WEEKLY_FLOOR_PCT}% weekly quota remaining"
        );
    }

    let selected = pick_best(
        &weekly_eligible,
        state,
        config.selected_account_name(provider),
    );
    log_selection(selected.entry, &available);
    Ok(selected)
}

fn selectable_scored_candidates<'a>(
    scored: &[(&'a AccountEntry, Option<AccountUsage>)],
    state: &QuotaState,
) -> Vec<(&'a AccountEntry, Option<AccountUsage>)> {
    scored
        .iter()
        .filter(|(entry, _)| !state.get(&entry.name).exhausted)
        .map(|(entry, usage)| (*entry, usage.clone()))
        .collect()
}

fn weekly_floor_candidates<'a>(
    scored: &[(&'a AccountEntry, Option<AccountUsage>)],
) -> Vec<(&'a AccountEntry, Option<AccountUsage>)> {
    scored
        .iter()
        .filter(|(_, usage)| {
            usage
                .as_ref()
                .is_none_or(|usage| usage.weekly_remaining_pct >= WEEKLY_FLOOR_PCT)
        })
        .map(|(entry, usage)| (*entry, usage.clone()))
        .collect()
}

fn low_weekly_account_summaries(scored: &[(&AccountEntry, Option<AccountUsage>)]) -> Vec<String> {
    scored
        .iter()
        .filter_map(|(entry, usage)| {
            usage.as_ref().and_then(|usage| {
                (usage.weekly_remaining_pct < WEEKLY_FLOOR_PCT)
                    .then(|| format!("{} ({}%)", entry.name, usage.weekly_remaining_pct))
            })
        })
        .collect()
}

/// Pure scoring logic, separated for testability.
fn pick_best<'a>(
    scored: &[(&'a AccountEntry, Option<AccountUsage>)],
    state: &QuotaState,
    preferred_name: Option<&str>,
) -> SelectedAccount<'a> {
    let scored_refs: Vec<_> = scored.iter().collect();

    let known_usable: Vec<_> = scored_refs
        .iter()
        .copied()
        .filter(|(_, usage)| usage.as_ref().is_some_and(usage_has_remaining))
        .collect();

    if !known_usable.is_empty() {
        let least_busy = minimum_active_leases(&known_usable, state);
        let least_busy_usable = with_active_leases(&known_usable, state, least_busy);
        return pick_best_by_health(&least_busy_usable, state, preferred_name);
    }

    let known_usage: Vec<_> = scored_refs
        .iter()
        .copied()
        .filter(|(_, usage)| usage.is_some())
        .collect();

    if !known_usage.is_empty() {
        let least_busy = minimum_active_leases(&known_usage, state);
        let least_busy_known = with_active_leases(&known_usage, state, least_busy);
        let (entry, _) = least_busy_known
            .iter()
            .max_by(|a, b| {
                let sa = a.1.as_ref().map_or(0, |u| u.session_remaining_pct);
                let sb = b.1.as_ref().map_or(0, |u| u.session_remaining_pct);
                let wa = a.1.as_ref().map_or(0, |u| u.weekly_remaining_pct);
                let wb = b.1.as_ref().map_or(0, |u| u.weekly_remaining_pct);
                sa.cmp(&sb)
                    .then_with(|| wa.cmp(&wb))
                    .then_with(|| compare_preferred(a.0, b.0, preferred_name))
                    .then_with(|| compare_lru_desc(a.0, b.0, state))
                    .then_with(|| b.0.name.cmp(&a.0.name))
            })
            .unwrap();
        return SelectedAccount { entry };
    }

    let unknown_usage: Vec<_> = scored_refs
        .iter()
        .copied()
        .filter(|(_, usage)| usage.is_none())
        .collect();

    if !unknown_usage.is_empty() {
        let least_busy = minimum_active_leases(&unknown_usage, state);
        let least_busy_unknown = with_active_leases(&unknown_usage, state, least_busy);
        let (entry, _) = least_busy_unknown
            .iter()
            .max_by(|a, b| {
                compare_preferred(a.0, b.0, preferred_name)
                    .then_with(|| compare_lru_then_name_desc(a.0, b.0, state))
            })
            .unwrap();
        return SelectedAccount { entry };
    }

    unreachable!("pick_best requires at least one scored account")
}

fn pick_best_by_health<'a>(
    candidates: &[&(&'a AccountEntry, Option<AccountUsage>)],
    state: &QuotaState,
    preferred_name: Option<&str>,
) -> SelectedAccount<'a> {
    let session_healthy: Vec<_> = candidates
        .iter()
        .copied()
        .filter(|(_, usage)| {
            usage
                .as_ref()
                .is_some_and(|usage| usage.session_remaining_pct >= SESSION_FLOOR_PCT)
        })
        .collect();

    if !session_healthy.is_empty() {
        return pick_best_by_weekly(&session_healthy, state, preferred_name);
    }

    let (entry, _) = candidates
        .iter()
        .max_by(|a, b| {
            let sa = a.1.as_ref().map_or(0, |u| u.session_remaining_pct);
            let sb = b.1.as_ref().map_or(0, |u| u.session_remaining_pct);
            let wa = a.1.as_ref().map_or(0, |u| u.weekly_remaining_pct);
            let wb = b.1.as_ref().map_or(0, |u| u.weekly_remaining_pct);
            sa.cmp(&sb)
                .then_with(|| wa.cmp(&wb))
                .then_with(|| compare_preferred(a.0, b.0, preferred_name))
                .then_with(|| compare_lru_desc(a.0, b.0, state))
                .then_with(|| b.0.name.cmp(&a.0.name))
        })
        .unwrap();

    SelectedAccount { entry }
}

fn pick_best_by_weekly<'a>(
    candidates: &[&(&'a AccountEntry, Option<AccountUsage>)],
    state: &QuotaState,
    preferred_name: Option<&str>,
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
                    .then_with(|| compare_preferred(a.0, b.0, preferred_name).reverse())
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
                .then_with(|| compare_preferred(a.0, b.0, preferred_name))
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

fn compare_preferred(
    a: &AccountEntry,
    b: &AccountEntry,
    preferred_name: Option<&str>,
) -> std::cmp::Ordering {
    match preferred_name {
        Some(preferred_name) => {
            let a_preferred = a.name == preferred_name;
            let b_preferred = b.name == preferred_name;
            a_preferred.cmp(&b_preferred)
        }
        None => std::cmp::Ordering::Equal,
    }
}

fn compare_lru_then_name_desc(
    a: &AccountEntry,
    b: &AccountEntry,
    state: &QuotaState,
) -> std::cmp::Ordering {
    compare_lru_desc(a, b, state).then_with(|| b.name.cmp(&a.name))
}

fn active_leases(entry: &AccountEntry, state: &QuotaState) -> u32 {
    state.get(&entry.name).active_leases
}

fn minimum_active_leases(
    scored: &[&(&AccountEntry, Option<AccountUsage>)],
    state: &QuotaState,
) -> u32 {
    scored
        .iter()
        .map(|(entry, _)| active_leases(entry, state))
        .min()
        .unwrap_or(0)
}

fn with_active_leases<'a, 'b>(
    scored: &'b [&'b (&'a AccountEntry, Option<AccountUsage>)],
    state: &QuotaState,
    active_lease_count: u32,
) -> Vec<&'b (&'a AccountEntry, Option<AccountUsage>)> {
    scored
        .iter()
        .copied()
        .filter(|(entry, _)| active_leases(entry, state) == active_lease_count)
        .collect()
}

fn usage_has_remaining(usage: &AccountUsage) -> bool {
    usage.session_remaining_pct > 0 && usage.weekly_remaining_pct > 0
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
    fn selectable_candidates_excludes_locally_exhausted_accounts() {
        let a = make_account("healthy", Provider::Claude);
        let b = make_account("cooling", Provider::Claude);
        let mut state = QuotaState::default();
        state.mark_exhausted("cooling", chrono::Utc::now());

        let scored = vec![(&a, Some(make_usage(50, 600, 50, 3600))), (&b, None)];
        let available = selectable_scored_candidates(&scored, &state);

        assert_eq!(available.len(), 1);
        assert_eq!(available[0].0.name, "healthy");
    }

    #[test]
    fn soonest_weekly_reset_wins_above_floor() {
        let a = make_account("fast-reset", Provider::Claude);
        let b = make_account("slow-reset", Provider::Claude);
        let state = QuotaState::default();

        // fast-reset: weekly resets in 3600s (1h)
        // slow-reset: weekly resets in 86400s (24h)
        // Both above 10% weekly floor
        let scored: Vec<(&AccountEntry, Option<AccountUsage>)> = vec![
            (&a, Some(make_usage(60, 600, 50, 3600))),
            (&b, Some(make_usage(30, 3600, 50, 86400))),
        ];

        let selected = pick_best(&scored, &state, None);
        assert_eq!(selected.entry.name, "fast-reset");
    }

    #[test]
    fn below_weekly_floor_accounts_are_not_selected() {
        let config = QuotaConfig {
            accounts: vec![
                make_account("healthy", Provider::Codex),
                make_account("low-weekly", Provider::Codex),
            ],
            selected_codex_account: None,
            selected_claude_account: None,
        };
        let state = QuotaState::default();

        let scored: Vec<(&AccountEntry, Option<AccountUsage>)> = vec![
            (&config.accounts[0], Some(make_usage(50, 600, 15, 3600))),
            (&config.accounts[1], Some(make_usage(1, 3600, 96, 100))),
        ];

        let selected =
            select_account_from_scores(&config, &state, Provider::Codex, &scored).unwrap();
        assert_eq!(selected.entry.name, "healthy");
    }

    #[test]
    fn below_weekly_floor_accounts_are_rejected_when_no_eligible_account_exists() {
        let config = QuotaConfig {
            accounts: vec![
                make_account("low-a", Provider::Codex),
                make_account("low-b", Provider::Codex),
            ],
            selected_codex_account: None,
            selected_claude_account: None,
        };
        let state = QuotaState::default();

        let scored: Vec<(&AccountEntry, Option<AccountUsage>)> = vec![
            (&config.accounts[0], Some(make_usage(50, 600, 92, 0))),
            (&config.accounts[1], Some(make_usage(50, 3600, 96, 0))),
        ];

        let error =
            select_account_from_scores(&config, &state, Provider::Codex, &scored).unwrap_err();
        assert!(error.to_string().contains("10% weekly quota"));
    }

    #[test]
    fn weekly_floor_beats_active_lease_balancing() {
        let config = QuotaConfig {
            accounts: vec![
                make_account("busy-healthy", Provider::Codex),
                make_account("idle-low-weekly", Provider::Codex),
            ],
            selected_codex_account: None,
            selected_claude_account: None,
        };
        let mut state = QuotaState::default();
        state.mark_selected("busy-healthy", chrono::Utc::now());

        let scored: Vec<(&AccountEntry, Option<AccountUsage>)> = vec![
            (&config.accounts[0], Some(make_usage(5, 600, 15, 3600))),
            (&config.accounts[1], Some(make_usage(1, 3600, 96, 100))),
        ];

        let selected =
            select_account_from_scores(&config, &state, Provider::Codex, &scored).unwrap();
        assert_eq!(selected.entry.name, "busy-healthy");
    }

    #[test]
    fn above_floor_beats_below_floor() {
        let a = make_account("healthy", Provider::Claude);
        let b = make_account("depleted", Provider::Claude);
        let state = QuotaState::default();

        // healthy: 20% weekly remaining (above 10%), resets in 3600s
        // depleted: 5% weekly remaining (below 10%), resets in 100s
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
    fn floor_boundary_exact_10_is_above() {
        let a = make_account("edge", Provider::Claude);
        let state = QuotaState::default();

        // Exactly 10% weekly remaining = above floor
        let scored: Vec<(&AccountEntry, Option<AccountUsage>)> =
            vec![(&a, Some(make_usage(50, 1000, 90, 86400)))];

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
    fn preferred_account_only_breaks_true_ties() {
        let a = make_account("preferred", Provider::Claude);
        let b = make_account("other", Provider::Claude);
        let state = QuotaState::default();

        let scored: Vec<(&AccountEntry, Option<AccountUsage>)> = vec![
            (&a, Some(make_usage(50, 600, 50, 3600))),
            (&b, Some(make_usage(50, 600, 50, 3600))),
        ];

        let selected = pick_best(&scored, &state, Some("preferred"));
        assert_eq!(selected.entry.name, "preferred");
    }

    #[test]
    fn healthier_account_beats_preferred_account() {
        let a = make_account("preferred", Provider::Claude);
        let b = make_account("other", Provider::Claude);
        let state = QuotaState::default();

        let scored: Vec<(&AccountEntry, Option<AccountUsage>)> = vec![
            (&a, Some(make_usage(50, 600, 95, 3600))),
            (&b, Some(make_usage(40, 600, 50, 7200))),
        ];

        let selected = pick_best(&scored, &state, Some("preferred"));
        assert_eq!(selected.entry.name, "other");
    }

    #[test]
    fn idle_usable_account_beats_busy_preferred_account() {
        let a = make_account("preferred", Provider::Claude);
        let b = make_account("other", Provider::Claude);
        let now = chrono::Utc::now();
        let mut state = QuotaState::default();
        state.mark_selected("preferred", now);

        let scored: Vec<(&AccountEntry, Option<AccountUsage>)> = vec![
            (&a, Some(make_usage(50, 600, 50, 3600))),
            (&b, Some(make_usage(70, 600, 88, 7200))),
        ];

        let selected = pick_best(&scored, &state, Some("preferred"));
        assert_eq!(selected.entry.name, "other");
    }
}
