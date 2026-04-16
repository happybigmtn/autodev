use std::fs;
use std::path::Path;

use anyhow::{Context, Result};
use base64::{engine::general_purpose, Engine as _};
use serde::Deserialize;

use crate::quota_config::Provider;

// OAuth token endpoints and client IDs
const CLAUDE_TOKEN_ENDPOINT: &str = "https://platform.claude.com/v1/oauth/token";
const CLAUDE_CLIENT_ID: &str = "9d1c250a-e61b-44d9-88ed-5944d1962f5e";
const CODEX_TOKEN_ENDPOINT: &str = "https://auth.openai.com/oauth/token";
const CODEX_CLIENT_ID: &str = "app_EMoamEEZ73f0CkXaXp7hrannk";

// Refresh 5 minutes before actual expiry
const REFRESH_BUFFER_SECS: i64 = 300;

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
    pub(crate) reset_after_seconds: u64,
}

// ── Claude usage ───────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub(crate) struct ClaudeUsageResponse {
    pub(crate) five_hour: ClaudeWindow,
    pub(crate) seven_day: ClaudeWindow,
}

#[derive(Debug, Deserialize)]
pub(crate) struct ClaudeWindow {
    pub(crate) utilization: f64,
    pub(crate) resets_at: Option<String>,
}

// ── Unified usage result ───────────────────────────────────────────────

#[derive(Clone, Debug)]
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

// ── Token refresh ─────────────────────────────────────────────────────

async fn refresh_claude_if_needed(profile_dir: &Path) -> Result<()> {
    let creds_path = profile_dir.join(".credentials.json");
    let creds_text = fs::read_to_string(&creds_path)
        .with_context(|| format!("failed to read {}", creds_path.display()))?;
    let creds: serde_json::Value = serde_json::from_str(&creds_text)
        .with_context(|| format!("failed to parse {}", creds_path.display()))?;

    let oauth = &creds["claudeAiOauth"];
    let Some(expires_at) = oauth["expiresAt"].as_i64() else {
        return Ok(());
    };
    let Some(refresh_token) = oauth["refreshToken"].as_str() else {
        return Ok(());
    };

    let now_ms = chrono::Utc::now().timestamp_millis();
    if now_ms < expires_at - (REFRESH_BUFFER_SECS * 1000) {
        return Ok(());
    }

    eprintln!("[quota-router] refreshing Claude OAuth token...");

    let scopes = oauth["scopes"]
        .as_array()
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str())
                .collect::<Vec<_>>()
                .join(" ")
        })
        .unwrap_or_else(|| {
            "user:profile user:inference user:sessions:claude_code \
             user:mcp_servers user:file_upload"
                .to_string()
        });

    let client = reqwest::Client::new();
    let resp = client
        .post(CLAUDE_TOKEN_ENDPOINT)
        .json(&serde_json::json!({
            "grant_type": "refresh_token",
            "refresh_token": refresh_token,
            "client_id": CLAUDE_CLIENT_ID,
            "scope": scopes,
        }))
        .send()
        .await
        .context("Claude token refresh request failed")?;

    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        anyhow::bail!("Claude token refresh returned {status}: {body}");
    }

    let token_resp: serde_json::Value = resp
        .json()
        .await
        .context("failed to parse Claude token response")?;

    let mut creds: serde_json::Value = serde_json::from_str(&creds_text)?;
    let oauth = creds["claudeAiOauth"]
        .as_object_mut()
        .context("claudeAiOauth is not an object")?;

    if let Some(at) = token_resp["access_token"].as_str() {
        oauth.insert("accessToken".into(), serde_json::json!(at));
    }
    if let Some(rt) = token_resp["refresh_token"].as_str() {
        oauth.insert("refreshToken".into(), serde_json::json!(rt));
    }
    if let Some(expires_in) = token_resp["expires_in"].as_i64() {
        let new_expires_at = chrono::Utc::now().timestamp_millis() + (expires_in * 1000);
        oauth.insert("expiresAt".into(), serde_json::json!(new_expires_at));
    }

    fs::write(&creds_path, serde_json::to_string(&creds)?.as_bytes())
        .with_context(|| format!("failed to write {}", creds_path.display()))?;

    eprintln!("[quota-router] Claude OAuth token refreshed");
    Ok(())
}

async fn refresh_codex_if_needed(profile_dir: &Path) -> Result<()> {
    let auth_path = profile_dir.join("auth.json");
    let auth_text = fs::read_to_string(&auth_path)
        .with_context(|| format!("failed to read {}", auth_path.display()))?;
    let auth: serde_json::Value = serde_json::from_str(&auth_text)
        .with_context(|| format!("failed to parse {}", auth_path.display()))?;

    let Some(access_token) = auth["tokens"]["access_token"].as_str() else {
        return Ok(());
    };
    let Some(refresh_token) = auth["tokens"]["refresh_token"].as_str() else {
        return Ok(());
    };

    if !jwt_expired(access_token) {
        return Ok(());
    }

    eprintln!("[quota-router] refreshing Codex OAuth token...");

    let client = reqwest::Client::new();
    let resp = client
        .post(CODEX_TOKEN_ENDPOINT)
        .form(&[
            ("grant_type", "refresh_token"),
            ("refresh_token", refresh_token),
            ("client_id", CODEX_CLIENT_ID),
        ])
        .send()
        .await
        .context("Codex token refresh request failed")?;

    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        anyhow::bail!("Codex token refresh returned {status}: {body}");
    }

    let token_resp: serde_json::Value = resp
        .json()
        .await
        .context("failed to parse Codex token response")?;

    let mut auth: serde_json::Value = serde_json::from_str(&auth_text)?;
    let tokens = auth["tokens"]
        .as_object_mut()
        .context("tokens is not an object")?;

    if let Some(at) = token_resp["access_token"].as_str() {
        tokens.insert("access_token".into(), serde_json::json!(at));
    }
    if let Some(rt) = token_resp["refresh_token"].as_str() {
        tokens.insert("refresh_token".into(), serde_json::json!(rt));
    }
    if let Some(id) = token_resp["id_token"].as_str() {
        tokens.insert("id_token".into(), serde_json::json!(id));
    }
    auth["last_refresh"] = serde_json::json!(chrono::Utc::now().to_rfc3339());

    fs::write(&auth_path, serde_json::to_string(&auth)?.as_bytes())
        .with_context(|| format!("failed to write {}", auth_path.display()))?;

    eprintln!("[quota-router] Codex OAuth token refreshed");
    Ok(())
}

/// Check if a JWT access token is expired (or will expire within the buffer).
fn jwt_expired(token: &str) -> bool {
    let Some(payload_b64) = token.split('.').nth(1) else {
        return true;
    };
    let Ok(payload_bytes) = general_purpose::URL_SAFE_NO_PAD.decode(payload_b64) else {
        return true;
    };
    let Ok(payload) = serde_json::from_slice::<serde_json::Value>(&payload_bytes) else {
        return true;
    };
    let Some(exp) = payload["exp"].as_i64() else {
        return true;
    };
    let now = chrono::Utc::now().timestamp();
    now >= exp - REFRESH_BUFFER_SECS
}

// ── Fetch functions ────────────────────────────────────────────────────

pub(crate) async fn fetch_codex_usage(profile_dir: &Path) -> Result<AccountUsage> {
    if let Err(e) = refresh_codex_if_needed(profile_dir).await {
        eprintln!("[quota-router] codex token refresh failed: {e:#}");
    }

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
    if let Err(e) = refresh_claude_if_needed(profile_dir).await {
        eprintln!("[quota-router] claude token refresh failed: {e:#}");
    }

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
    let session_reset_secs = usage
        .five_hour
        .resets_at
        .as_deref()
        .map(parse_reset_secs)
        .unwrap_or(0);
    let weekly_reset_secs = usage
        .seven_day
        .resets_at
        .as_deref()
        .map(parse_reset_secs)
        .unwrap_or(0);

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

pub(crate) async fn fetch_usage(provider: Provider, profile_dir: &Path) -> Result<AccountUsage> {
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
