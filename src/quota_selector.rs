use std::fs;
use std::path::Path;
use std::sync::{Mutex, OnceLock};

use anyhow::{bail, Result};
use sha2::{Digest, Sha256};

use crate::quota_config::{codex_live_home, AccountEntry, Provider, QuotaConfig};
use crate::quota_state::QuotaState;
use crate::quota_usage::{self, AccountUsage};

/// Accounts with less than this weekly remaining are in the "low" tier.
const WEEKLY_FLOOR_PCT: u32 = 10;
/// Accounts with less than this session remaining are avoided when possible.
const SESSION_FLOOR_PCT: u32 = 25;

/// Every configured account for the provider was excluded because its
/// credentials are revoked/expired. Typed so exec seams can distinguish
/// "the router has nothing usable" (fall back to the provider's default
/// login) from a transient routing failure (propagate).
#[derive(Debug)]
pub(crate) struct AllAccountsInvalid {
    pub(crate) provider: Provider,
    /// True when no live probes were needed because the unchanged pool was
    /// already proven invalid earlier in this process.
    pub(crate) cached: bool,
}

impl std::fmt::Display for AllAccountsInvalid {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "all {} accounts have invalid credentials (revoked/expired). \
             Run `codex login` then `auto quota accounts capture <name>` for at least one.",
            self.provider
        )
    }
}

impl std::error::Error for AllAccountsInvalid {}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct PoolFingerprint([u8; 32]);

enum PoolEvaluation {
    Evaluating(tokio::sync::watch::Sender<PoolEvaluationResult>),
    Invalid,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum PoolEvaluationResult {
    Pending,
    Invalid,
    RetryIndependently,
}

struct ProviderPoolState {
    fingerprint: PoolFingerprint,
    evaluation: PoolEvaluation,
}

#[derive(Default)]
struct InvalidPoolCache {
    claude: Option<ProviderPoolState>,
    codex: Option<ProviderPoolState>,
}

impl InvalidPoolCache {
    fn get_mut(&mut self, provider: Provider) -> &mut Option<ProviderPoolState> {
        match provider {
            Provider::Claude => &mut self.claude,
            Provider::Codex => &mut self.codex,
        }
    }
}

enum PoolEvaluationAction {
    Evaluate,
    Wait(tokio::sync::watch::Receiver<PoolEvaluationResult>),
    UseCachedInvalid,
}

fn invalid_pool_cache() -> &'static Mutex<InvalidPoolCache> {
    static CACHE: OnceLock<Mutex<InvalidPoolCache>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(InvalidPoolCache::default()))
}

/// Hash a credential path's identity and content without ever retaining or
/// logging credential bytes. Metadata makes a same-content rewrite invalidate
/// the cache too, while content catches in-place updates on coarse-mtime filesystems.
fn hash_path_state(hasher: &mut Sha256, path: &Path) {
    hasher.update(path.to_string_lossy().as_bytes());
    match fs::symlink_metadata(path) {
        Ok(metadata) => {
            hasher.update(b"present");
            hasher.update(metadata.len().to_le_bytes());
            if let Ok(modified) = metadata.modified() {
                if let Ok(duration) = modified.duration_since(std::time::UNIX_EPOCH) {
                    hasher.update(duration.as_secs().to_le_bytes());
                    hasher.update(duration.subsec_nanos().to_le_bytes());
                }
            }
            if metadata.file_type().is_symlink() {
                hasher.update(b"symlink");
                if let Ok(target) = fs::read_link(path) {
                    hasher.update(target.to_string_lossy().as_bytes());
                }
            } else if metadata.is_file() {
                hasher.update(b"file");
                match fs::read(path) {
                    Ok(bytes) => hasher.update(Sha256::digest(bytes)),
                    Err(error) => hasher.update(error.kind().to_string().as_bytes()),
                }
            } else {
                hasher.update(b"other");
            }
        }
        Err(error) => {
            hasher.update(b"missing-or-unreadable");
            hasher.update(error.kind().to_string().as_bytes());
        }
    }
}

fn pool_fingerprint(config: &QuotaConfig, provider: Provider) -> Result<PoolFingerprint> {
    let mut hasher = Sha256::new();
    hasher.update(provider.label().as_bytes());
    // Hash the on-disk config as well as the parsed account paths. This makes
    // every operator config edit an explicit invalidation event.
    hash_path_state(&mut hasher, &QuotaConfig::config_path());

    for entry in config.accounts_for_provider(provider) {
        hasher.update(entry.name.as_bytes());
        hasher.update([u8::from(entry.live)]);
        let profile_dir = usage_profile_dir(provider, entry)?;
        match provider {
            Provider::Codex => {
                // Include both supported layouts so creating/removing the
                // isolated home invalidates even when resolution changes.
                hash_path_state(&mut hasher, &profile_dir.join("auth.json"));
                hash_path_state(
                    &mut hasher,
                    &profile_dir.join("codex-home").join("auth.json"),
                );
            }
            Provider::Claude => {
                hash_path_state(&mut hasher, &profile_dir.join(".credentials.json"));
            }
        }
    }

    Ok(PoolFingerprint(hasher.finalize().into()))
}

/// Join or lead the evaluation for one provider/fingerprint. The mutex is held
/// only while changing this tiny process-local state; live provider I/O happens
/// after it is released, and Claude never waits on Codex (or vice versa).
fn begin_pool_evaluation(provider: Provider, fingerprint: PoolFingerprint) -> PoolEvaluationAction {
    let mut cache = invalid_pool_cache()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let slot = cache.get_mut(provider);

    if let Some(state) = slot.as_ref() {
        if state.fingerprint == fingerprint {
            return match &state.evaluation {
                PoolEvaluation::Invalid => PoolEvaluationAction::UseCachedInvalid,
                PoolEvaluation::Evaluating(done) => PoolEvaluationAction::Wait(done.subscribe()),
            };
        }
    }

    // A changed config/credential fingerprint starts a new generation. Wake
    // waiters for the superseded generation so they can recompute and join it.
    if let Some(ProviderPoolState {
        evaluation: PoolEvaluation::Evaluating(done),
        ..
    }) = slot.as_ref()
    {
        let _ = done.send(PoolEvaluationResult::RetryIndependently);
    }
    let (done, _receiver) = tokio::sync::watch::channel(PoolEvaluationResult::Pending);
    *slot = Some(ProviderPoolState {
        fingerprint,
        evaluation: PoolEvaluation::Evaluating(done),
    });
    PoolEvaluationAction::Evaluate
}

/// Publish an evaluation outcome only if its fingerprint is still current.
/// `invalid=false` removes the entry: successful/unknown pools are never cached.
fn finish_pool_evaluation(provider: Provider, fingerprint: PoolFingerprint, invalid: bool) {
    let mut cache = invalid_pool_cache()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let slot = cache.get_mut(provider);
    let Some(state) = slot.as_ref() else {
        return;
    };
    if state.fingerprint != fingerprint {
        return;
    }
    if let PoolEvaluation::Evaluating(done) = &state.evaluation {
        let outcome = if invalid {
            PoolEvaluationResult::Invalid
        } else {
            PoolEvaluationResult::RetryIndependently
        };
        let _ = done.send(outcome);
    }
    if invalid {
        *slot = Some(ProviderPoolState {
            fingerprint,
            evaluation: PoolEvaluation::Invalid,
        });
    } else {
        *slot = None;
    }
}

/// Cancellation-safe ownership of a live pool evaluation. If the selecting
/// future is dropped while provider I/O is pending, peers are awakened and a
/// later caller may retry instead of waiting forever on an abandoned leader.
struct PoolEvaluationGuard {
    provider: Provider,
    fingerprint: PoolFingerprint,
    finished: bool,
}

impl PoolEvaluationGuard {
    fn new(provider: Provider, fingerprint: PoolFingerprint) -> Self {
        Self {
            provider,
            fingerprint,
            finished: false,
        }
    }

    fn finish(mut self, invalid: bool) {
        finish_pool_evaluation(self.provider, self.fingerprint, invalid);
        self.finished = true;
    }
}

impl Drop for PoolEvaluationGuard {
    fn drop(&mut self) {
        if !self.finished {
            finish_pool_evaluation(self.provider, self.fingerprint, false);
        }
    }
}

#[cfg(test)]
fn forget_invalid_pool(provider: Provider) {
    let mut cache = invalid_pool_cache()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let slot = cache.get_mut(provider);
    if let Some(ProviderPoolState {
        evaluation: PoolEvaluation::Evaluating(done),
        ..
    }) = slot.as_ref()
    {
        let _ = done.send(PoolEvaluationResult::RetryIndependently);
    }
    *slot = None;
}

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

    if std::env::var_os("AUTO_QUOTA_SKIP_USAGE").as_deref() == Some(std::ffi::OsStr::new("1")) {
        return Ok(candidates.into_iter().map(|entry| (entry, None)).collect());
    }

    let evaluation_guard = loop {
        let fingerprint = pool_fingerprint(config, provider)?;
        match begin_pool_evaluation(provider, fingerprint) {
            PoolEvaluationAction::Evaluate => {
                break Some(PoolEvaluationGuard::new(provider, fingerprint));
            }
            PoolEvaluationAction::UseCachedInvalid => {
                return Err(anyhow::Error::new(AllAccountsInvalid {
                    provider,
                    cached: true,
                }));
            }
            PoolEvaluationAction::Wait(mut done) => {
                if *done.borrow() == PoolEvaluationResult::Pending {
                    let _ = done.changed().await;
                }
                match *done.borrow() {
                    PoolEvaluationResult::Invalid => {
                        return Err(anyhow::Error::new(AllAccountsInvalid {
                            provider,
                            cached: true,
                        }));
                    }
                    PoolEvaluationResult::RetryIndependently => {
                        // The leader found at least one candidate (or was
                        // cancelled). Do not serialize healthy selectors: this
                        // caller may perform its own normal live evaluation.
                        break None;
                    }
                    PoolEvaluationResult::Pending => {
                        // A superseded/closed generation: recompute its
                        // fingerprint and join the current generation.
                    }
                }
            }
        }
    };

    let mut scored: Vec<(&AccountEntry, Option<AccountUsage>)> =
        Vec::with_capacity(candidates.len());
    for entry in candidates {
        let profile_dir = match usage_profile_dir(provider, entry) {
            Ok(profile_dir) => profile_dir,
            Err(error) => return Err(error),
        };
        match quota_usage::fetch_usage(provider, &profile_dir).await {
            Ok(usage) => scored.push((entry, Some(usage))),
            Err(e) => {
                eprintln!(
                    "[quota-router] failed to fetch usage for '{}': {}",
                    entry.name,
                    quota_usage::sanitize_quota_error_message(&e),
                );
                if quota_usage::is_auth_failure(&e) {
                    // Revoked/terminated credentials: exclude entirely so the
                    // router never swaps in a dead profile (which clobbers the
                    // live provider login).
                    let recovery = match provider {
                        Provider::Codex => {
                            format!("run `auto quota accounts login {}`", entry.name)
                        }
                        Provider::Claude => {
                            format!(
                                "run `claude login` then `auto quota accounts capture {}`",
                                entry.name
                            )
                        }
                    };
                    eprintln!(
                        "[quota-router] excluding '{}': credentials invalid; {recovery}",
                        entry.name
                    );
                    continue;
                }
                scored.push((entry, None));
            }
        }
    }

    if scored.is_empty() {
        if let Some(evaluation_guard) = evaluation_guard {
            evaluation_guard.finish(true);
        }
        return Err(anyhow::Error::new(AllAccountsInvalid {
            provider,
            cached: false,
        }));
    }

    // This is a negative-only cache. A pool that produces any candidate must
    // leave no remembered failure behind.
    if let Some(evaluation_guard) = evaluation_guard {
        evaluation_guard.finish(false);
    }

    Ok(scored)
}

fn usage_profile_dir(provider: Provider, entry: &AccountEntry) -> Result<std::path::PathBuf> {
    if matches!(provider, Provider::Codex) && entry.live {
        Ok(codex_live_home())
    } else {
        QuotaConfig::profile_dir(provider, &entry.name)
    }
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

/// True when an account is eligible on the weekly axis: either its weekly
/// usage is unknown (no weekly window observed — absent ≠ exhausted, so do NOT
/// gate) or a PRESENT weekly window still has at least the floor remaining.
fn weekly_axis_eligible(usage: &AccountUsage) -> bool {
    !usage.weekly_known || usage.weekly_remaining_pct >= WEEKLY_FLOOR_PCT
}

/// Sort key for "weekly resets soonest". Unknown weekly usage has no
/// meaningful reset time, so it sorts last and is never falsely preferred over
/// an account whose weekly window we actually observed.
fn weekly_reset_key(usage: &AccountUsage) -> u64 {
    if usage.weekly_known {
        usage.weekly_resets_in_secs
    } else {
        u64::MAX
    }
}

fn weekly_floor_candidates<'a>(
    scored: &[(&'a AccountEntry, Option<AccountUsage>)],
) -> Vec<(&'a AccountEntry, Option<AccountUsage>)> {
    scored
        .iter()
        .filter(|(_, usage)| usage.as_ref().is_none_or(weekly_axis_eligible))
        .map(|(entry, usage)| (*entry, usage.clone()))
        .collect()
}

fn low_weekly_account_summaries(scored: &[(&AccountEntry, Option<AccountUsage>)]) -> Vec<String> {
    scored
        .iter()
        .filter_map(|(entry, usage)| {
            usage.as_ref().and_then(|usage| {
                (usage.weekly_known && usage.weekly_remaining_pct < WEEKLY_FLOOR_PCT)
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
        .filter(|(_, u)| u.as_ref().is_some_and(weekly_axis_eligible))
        .collect();

    if !above_weekly_floor.is_empty() {
        let (entry, _) = above_weekly_floor
            .iter()
            .min_by(|a, b| {
                let ra = a.1.as_ref().map_or(u64::MAX, weekly_reset_key);
                let rb = b.1.as_ref().map_or(u64::MAX, weekly_reset_key);
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
    // Unknown weekly usage must not read as "no weekly remaining" — that would
    // drop a healthy account out of the primary `known_usable` tier.
    usage.session_remaining_pct > 0 && (!usage.weekly_known || usage.weekly_remaining_pct > 0)
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

    #[cfg(unix)]
    struct TempPoolHome {
        root: std::path::PathBuf,
        previous_home: Option<std::ffi::OsString>,
        previous_config_home: Option<std::ffi::OsString>,
        _env_lock: std::sync::MutexGuard<'static, ()>,
    }

    #[cfg(unix)]
    impl TempPoolHome {
        fn new() -> Self {
            let env_lock = crate::util::test_process_env_lock()
                .lock()
                .expect("failed to lock process env");
            let stamp = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("system time should be after unix epoch")
                .as_nanos();
            let root = std::env::temp_dir().join(format!(
                "autodev-quota-negative-cache-{}-{stamp}",
                std::process::id()
            ));
            let home = root.join("home");
            let config_home = root.join("config");
            fs::create_dir_all(&home).expect("failed to create temp home");
            fs::create_dir_all(&config_home).expect("failed to create temp config home");
            let previous_home = std::env::var_os("HOME");
            let previous_config_home = std::env::var_os("XDG_CONFIG_HOME");
            std::env::set_var("HOME", home);
            std::env::set_var("XDG_CONFIG_HOME", config_home);
            Self {
                root,
                previous_home,
                previous_config_home,
                _env_lock: env_lock,
            }
        }
    }

    #[cfg(unix)]
    impl Drop for TempPoolHome {
        fn drop(&mut self) {
            if let Some(previous) = &self.previous_home {
                std::env::set_var("HOME", previous);
            } else {
                std::env::remove_var("HOME");
            }
            if let Some(previous) = &self.previous_config_home {
                std::env::set_var("XDG_CONFIG_HOME", previous);
            } else {
                std::env::remove_var("XDG_CONFIG_HOME");
            }
            let _ = fs::remove_dir_all(&self.root);
        }
    }

    fn make_account(name: &str, provider: Provider) -> AccountEntry {
        AccountEntry {
            name: name.into(),
            provider,
            live: false,
        }
    }

    #[cfg(unix)]
    #[test]
    fn invalid_pool_cache_is_reused_then_invalidated_by_credentials_and_config() {
        let _home = TempPoolHome::new();
        let mut config = QuotaConfig::default();
        config
            .add_account(make_account("dead", Provider::Codex))
            .expect("failed to add codex account");
        config.save().expect("failed to save quota config");

        let profile_dir = QuotaConfig::profile_dir(Provider::Codex, "dead")
            .expect("failed to resolve profile dir");
        fs::create_dir_all(&profile_dir).expect("failed to create profile dir");
        let auth_path = profile_dir.join("auth.json");
        fs::write(&auth_path, br#"{"token":"revoked"}"#)
            .expect("failed to write initial credentials");

        let initial =
            pool_fingerprint(&config, Provider::Codex).expect("failed to fingerprint initial pool");
        assert!(matches!(
            begin_pool_evaluation(Provider::Codex, initial),
            PoolEvaluationAction::Evaluate
        ));
        finish_pool_evaluation(Provider::Codex, initial, true);
        assert!(matches!(
            begin_pool_evaluation(Provider::Codex, initial),
            PoolEvaluationAction::UseCachedInvalid
        ));

        fs::write(&auth_path, br#"{"token":"new-login"}"#).expect("failed to update credentials");
        let after_credentials = pool_fingerprint(&config, Provider::Codex)
            .expect("failed to fingerprint updated credentials");
        assert_ne!(initial, after_credentials);
        assert!(matches!(
            begin_pool_evaluation(Provider::Codex, after_credentials),
            PoolEvaluationAction::Evaluate
        ));

        finish_pool_evaluation(Provider::Codex, after_credentials, true);
        config
            .add_account(make_account("config-change", Provider::Claude))
            .expect("failed to update quota config");
        config.save().expect("failed to save updated quota config");
        let after_config = pool_fingerprint(&config, Provider::Codex)
            .expect("failed to fingerprint updated config");
        assert_ne!(after_credentials, after_config);
        assert!(matches!(
            begin_pool_evaluation(Provider::Codex, after_config),
            PoolEvaluationAction::Evaluate
        ));

        finish_pool_evaluation(Provider::Codex, after_config, true);
        forget_invalid_pool(Provider::Codex);
        assert!(matches!(
            begin_pool_evaluation(Provider::Codex, after_config),
            PoolEvaluationAction::Evaluate
        ));
        finish_pool_evaluation(Provider::Codex, after_config, false);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn simultaneous_selectors_single_flight_one_invalid_pool_evaluation() {
        const SELECTORS: usize = 8;
        let fingerprint = PoolFingerprint([0x5a; 32]);
        forget_invalid_pool(Provider::Claude);

        let start = std::sync::Arc::new(tokio::sync::Barrier::new(SELECTORS));
        let joined_evaluation = std::sync::Arc::new(tokio::sync::Barrier::new(SELECTORS));
        let evaluations = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let waiters = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let mut selectors = Vec::with_capacity(SELECTORS);
        for _ in 0..SELECTORS {
            let start = std::sync::Arc::clone(&start);
            let joined_evaluation = std::sync::Arc::clone(&joined_evaluation);
            let evaluations = std::sync::Arc::clone(&evaluations);
            let waiters = std::sync::Arc::clone(&waiters);
            selectors.push(tokio::spawn(async move {
                start.wait().await;
                loop {
                    match begin_pool_evaluation(Provider::Claude, fingerprint) {
                        PoolEvaluationAction::Evaluate => {
                            evaluations.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                            // The leader cannot publish until every peer has
                            // deterministically observed the in-flight state.
                            joined_evaluation.wait().await;
                            finish_pool_evaluation(Provider::Claude, fingerprint, true);
                            break AllAccountsInvalid {
                                provider: Provider::Claude,
                                cached: false,
                            };
                        }
                        PoolEvaluationAction::Wait(mut done) => {
                            waiters.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                            joined_evaluation.wait().await;
                            if *done.borrow() == PoolEvaluationResult::Pending {
                                let _ = done.changed().await;
                            }
                            if *done.borrow() == PoolEvaluationResult::Invalid {
                                break AllAccountsInvalid {
                                    provider: Provider::Claude,
                                    cached: true,
                                };
                            }
                        }
                        PoolEvaluationAction::UseCachedInvalid => {
                            break AllAccountsInvalid {
                                provider: Provider::Claude,
                                cached: true,
                            };
                        }
                    }
                }
            }));
        }

        let mut fresh_warning_cycles = 0;
        let mut safe_cached_fallbacks = 0;
        for selector in selectors {
            let invalid = selector.await.expect("selector task should join");
            assert_eq!(invalid.provider, Provider::Claude);
            if invalid.cached {
                safe_cached_fallbacks += 1;
            } else {
                fresh_warning_cycles += 1;
            }
        }

        assert_eq!(evaluations.load(std::sync::atomic::Ordering::SeqCst), 1);
        assert_eq!(
            waiters.load(std::sync::atomic::Ordering::SeqCst),
            SELECTORS - 1
        );
        assert_eq!(fresh_warning_cycles, 1);
        assert_eq!(safe_cached_fallbacks, SELECTORS - 1);
        forget_invalid_pool(Provider::Claude);
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
            weekly_known: true,
            limit_reached: false,
        }
    }

    /// Usage with an observed session window but no weekly window (e.g. the
    /// Codex Pro plan when its single window is classified as session, or any
    /// response that omits the weekly budget). Weekly usage is unknown.
    fn make_usage_unknown_weekly(
        session_used_pct: u32,
        session_resets_in_secs: u64,
    ) -> AccountUsage {
        AccountUsage {
            plan: "test".into(),
            session_used_pct,
            session_remaining_pct: 100u32.saturating_sub(session_used_pct),
            session_resets_in_secs,
            weekly_used_pct: 0,
            weekly_remaining_pct: 100,
            weekly_resets_in_secs: 0,
            weekly_known: false,
            limit_reached: false,
        }
    }

    #[test]
    fn selectable_candidates_excludes_locally_exhausted_accounts() {
        let a = make_account("healthy", Provider::Claude);
        let b = make_account("cooling", Provider::Claude);
        let mut state = QuotaState::default();
        state.mark_exhausted("cooling", chrono::Utc::now()).unwrap();

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
    fn unknown_weekly_accounts_are_eligible_not_gated() {
        // Regression: Codex Pro accounts expose no weekly window, so weekly is
        // unknown. Such accounts MUST NOT be excluded from selection.
        let config = QuotaConfig {
            accounts: vec![
                make_account("happy", Provider::Codex),
                make_account("reachy", Provider::Codex),
            ],
            selected_codex_account: None,
            selected_claude_account: None,
        };
        let state = QuotaState::default();

        let scored: Vec<(&AccountEntry, Option<AccountUsage>)> = vec![
            (
                &config.accounts[0],
                Some(make_usage_unknown_weekly(8, 574224)),
            ),
            (
                &config.accounts[1],
                Some(make_usage_unknown_weekly(0, 604800)),
            ),
        ];

        // Must not bail with "no selectable account has ... weekly quota".
        let selected =
            select_account_from_scores(&config, &state, Provider::Codex, &scored).unwrap();
        assert!(selected.entry.name == "happy" || selected.entry.name == "reachy");
    }

    #[test]
    fn unknown_weekly_is_not_flagged_as_low_weekly() {
        let a = make_account("pro", Provider::Codex);
        let scored: Vec<(&AccountEntry, Option<AccountUsage>)> =
            vec![(&a, Some(make_usage_unknown_weekly(50, 600)))];
        assert!(low_weekly_account_summaries(&scored).is_empty());
    }

    #[test]
    fn unknown_weekly_stays_in_known_usable_tier() {
        // usage_has_remaining must not read unknown weekly as "0% weekly left".
        let usage = make_usage_unknown_weekly(20, 600);
        assert!(usage_has_remaining(&usage));
    }

    #[test]
    fn known_weekly_preferred_over_unknown_weekly_when_both_eligible() {
        // Among eligible accounts, the one whose weekly window we actually
        // observed (finite reset) should win over an unknown-weekly account,
        // rather than the unknown account being falsely treated as
        // "resets soonest" (reset 0).
        let known = make_account("known", Provider::Codex);
        let unknown = make_account("unknown", Provider::Codex);
        let state = QuotaState::default();

        let scored: Vec<(&AccountEntry, Option<AccountUsage>)> = vec![
            (&known, Some(make_usage(50, 600, 40, 86400))),
            (&unknown, Some(make_usage_unknown_weekly(50, 600))),
        ];

        let selected = pick_best(&scored, &state, None);
        assert_eq!(selected.entry.name, "known");
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
    fn live_codex_usage_reads_from_live_home() {
        // `codex_live_home()` reads the process-global `$HOME`, which sibling
        // quota tests mutate. Serialize against them via the shared env lock so
        // a concurrent `set_var`/`remove_var("HOME")` cannot race this read.
        let _env_lock = crate::util::test_process_env_lock()
            .lock()
            .expect("failed to lock process env");
        let live = AccountEntry {
            name: "live".into(),
            provider: Provider::Codex,
            live: true,
        };
        let captured = make_account("captured", Provider::Codex);

        assert_eq!(
            usage_profile_dir(Provider::Codex, &live).unwrap(),
            codex_live_home()
        );
        assert_eq!(
            usage_profile_dir(Provider::Codex, &captured).unwrap(),
            QuotaConfig::profile_dir(Provider::Codex, "captured").unwrap()
        );
    }

    #[test]
    fn live_account_can_be_selected_as_candidate() {
        let config = QuotaConfig {
            accounts: vec![
                make_account("captured", Provider::Codex),
                AccountEntry {
                    name: "live".into(),
                    provider: Provider::Codex,
                    live: true,
                },
            ],
            selected_codex_account: Some("live".into()),
            selected_claude_account: None,
        };
        let state = QuotaState::default();
        let scored: Vec<(&AccountEntry, Option<AccountUsage>)> = vec![
            (&config.accounts[0], Some(make_usage(50, 600, 50, 3600))),
            (&config.accounts[1], Some(make_usage(50, 600, 50, 3600))),
        ];

        let selected =
            select_account_from_scores(&config, &state, Provider::Codex, &scored).unwrap();
        assert_eq!(selected.entry.name, "live");
        assert!(selected.entry.live);
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
        state
            .mark_selected("busy-healthy", chrono::Utc::now())
            .unwrap();

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
        state.mark_used("alpha", t2).unwrap(); // more recent
        state.mark_used("beta", t1).unwrap(); // less recent

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
        state.mark_selected("preferred", now).unwrap();

        let scored: Vec<(&AccountEntry, Option<AccountUsage>)> = vec![
            (&a, Some(make_usage(50, 600, 50, 3600))),
            (&b, Some(make_usage(70, 600, 88, 7200))),
        ];

        let selected = pick_best(&scored, &state, Some("preferred"));
        assert_eq!(selected.entry.name, "other");
    }
}
