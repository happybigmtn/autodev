use std::fs;
use std::path::Path;

use anyhow::{Context, Result};
use serde::Deserialize;

use crate::quota_config::Provider;

// ── Codex (ChatGPT) usage ──────────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub(crate) struct CodexUsageResponse {
    pub(crate) plan_type: String,
    pub(crate) rate_limit: CodexRateLimit,
}

#[derive(Debug, Deserialize)]
pub(crate) struct CodexRateLimit {
    pub(crate) limit_reached: bool,
    pub(crate) primary_window: CodexWindow,
    pub(crate) secondary_window: Option<CodexWindow>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct CodexWindow {
    pub(crate) used_percent: u32,
    pub(crate) limit_window_seconds: u64,
    pub(crate) reset_after_seconds: u64,
}

// ── Claude usage ───────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub(crate) struct ClaudeUsageResponse {
    pub(crate) five_hour: ClaudeWindow,
    pub(crate) seven_day: ClaudeWindow,
    pub(crate) extra_usage: Option<ClaudeExtraUsage>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct ClaudeWindow {
    pub(crate) utilization: f64,
    pub(crate) resets_at: String,
}

#[derive(Debug, Deserialize)]
pub(crate) struct ClaudeExtraUsage {
    pub(crate) is_enabled: bool,
    pub(crate) used_credits: f64,
    pub(crate) monthly_limit: Option<f64>,
}

// ── Unified usage result ───────────────────────────────────────────────

#[derive(Debug)]
pub(crate) struct AccountUsage {
    pub(crate) plan: String,
    pub(crate) session_used_pct: u32,
    pub(crate) session_remaining_pct: u32,
    pub(crate) session_resets_in_secs: u64,
    pub(crate) weekly_used_pct: u32,
    pub(crate) weekly_remaining_pct: u32,
    pub(crate) weekly_resets_in_secs: u64,
    pub(crate) limit_reached: bool,
}

// ── Fetch functions ────────────────────────────────────────────────────

pub(crate) async fn fetch_codex_usage(profile_dir: &Path) -> Result<AccountUsage> {
    let auth_path = profile_dir.join("auth.json");
    let auth_text = fs::read_to_string(&auth_path)
        .with_context(|| format!("failed to read {}", auth_path.display()))?;
    let auth: serde_json::Value = serde_json::from_str(&auth_text)
        .with_context(|| format!("failed to parse {}", auth_path.display()))?;

    let access_token = auth["tokens"]["access_token"]
        .as_str()
        .context("missing tokens.access_token in codex auth")?;
    let account_id = auth["tokens"]["account_id"]
        .as_str()
        .context("missing tokens.account_id in codex auth")?;

    let client = reqwest::Client::new();
    let resp = client
        .get("https://chatgpt.com/backend-api/wham/usage")
        .header("Authorization", format!("Bearer {access_token}"))
        .header("chatgpt-account-id", account_id)
        .send()
        .await
        .context("failed to reach ChatGPT usage endpoint")?;

    if !resp.status().is_success() {
        anyhow::bail!("ChatGPT usage API returned {}", resp.status());
    }

    let usage: CodexUsageResponse = resp.json().await.context("failed to parse ChatGPT usage")?;

    let session_used = usage.rate_limit.primary_window.used_percent;
    let session_reset = usage.rate_limit.primary_window.reset_after_seconds;
    let (weekly_used, weekly_reset) = match &usage.rate_limit.secondary_window {
        Some(w) => (w.used_percent, w.reset_after_seconds),
        // No weekly window means no weekly budget — treat as fully consumed
        None => (100, 0),
    };

    Ok(AccountUsage {
        plan: usage.plan_type,
        session_used_pct: session_used,
        session_remaining_pct: 100u32.saturating_sub(session_used),
        session_resets_in_secs: session_reset,
        weekly_used_pct: weekly_used,
        weekly_remaining_pct: 100u32.saturating_sub(weekly_used),
        weekly_resets_in_secs: weekly_reset,
        limit_reached: usage.rate_limit.limit_reached,
    })
}

pub(crate) async fn fetch_claude_usage(profile_dir: &Path) -> Result<AccountUsage> {
    let creds_path = profile_dir.join(".credentials.json");
    let creds_text = fs::read_to_string(&creds_path)
        .with_context(|| format!("failed to read {}", creds_path.display()))?;
    let creds: serde_json::Value = serde_json::from_str(&creds_text)
        .with_context(|| format!("failed to parse {}", creds_path.display()))?;

    let access_token = creds["claudeAiOauth"]["accessToken"]
        .as_str()
        .context("missing claudeAiOauth.accessToken in claude credentials")?;

    let client = reqwest::Client::new();
    let resp = client
        .get("https://api.anthropic.com/api/oauth/usage")
        .header("Authorization", format!("Bearer {access_token}"))
        .header("anthropic-beta", "oauth-2025-04-20")
        .send()
        .await
        .context("failed to reach Claude usage endpoint")?;

    if !resp.status().is_success() {
        anyhow::bail!("Claude usage API returned {}", resp.status());
    }

    let usage: ClaudeUsageResponse = resp.json().await.context("failed to parse Claude usage")?;

    let session_used = usage.five_hour.utilization.round() as u32;
    let weekly_used = usage.seven_day.utilization.round() as u32;

    // Parse resets_at to get seconds remaining
    let session_reset_secs = parse_reset_secs(&usage.five_hour.resets_at);
    let weekly_reset_secs = parse_reset_secs(&usage.seven_day.resets_at);

    let limit_reached = session_used >= 100 || weekly_used >= 100;

    Ok(AccountUsage {
        plan: "max".into(),
        session_used_pct: session_used,
        session_remaining_pct: 100u32.saturating_sub(session_used),
        session_resets_in_secs: session_reset_secs,
        weekly_used_pct: weekly_used,
        weekly_remaining_pct: 100u32.saturating_sub(weekly_used),
        weekly_resets_in_secs: weekly_reset_secs,
        limit_reached,
    })
}

pub(crate) async fn fetch_usage(
    provider: Provider,
    profile_dir: &Path,
) -> Result<AccountUsage> {
    match provider {
        Provider::Codex => fetch_codex_usage(profile_dir).await,
        Provider::Claude => fetch_claude_usage(profile_dir).await,
    }
}

fn parse_reset_secs(resets_at: &str) -> u64 {
    chrono::DateTime::parse_from_rfc3339(resets_at)
        .map(|dt| {
            let now = chrono::Utc::now();
            let diff = dt.signed_duration_since(now);
            diff.num_seconds().max(0) as u64
        })
        .unwrap_or(0)
}
