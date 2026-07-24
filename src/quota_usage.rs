use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use base64::{engine::general_purpose, Engine as _};
use serde::Deserialize;

use crate::quota_config::{codex_home_for_profile, Provider};
use crate::util::{spawn_retrying_etxtbsy, write_0o600_if_unix};

// OAuth token endpoints and client IDs
const CLAUDE_TOKEN_ENDPOINT: &str = "https://platform.claude.com/v1/oauth/token";
const CLAUDE_CLIENT_ID: &str = "9d1c250a-e61b-44d9-88ed-5944d1962f5e";

// Refresh 5 minutes before actual expiry
const REFRESH_BUFFER_SECS: i64 = 300;
const CODEX_REFRESH_PROMPT: &str = "Reply with OK only.";
const CODEX_REFRESH_REASONING_EFFORT: &str = "low";
const CODEX_REFRESH_TIMEOUT: Duration = Duration::from_secs(20);

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
    /// Duration of this rate-limit window in seconds. Used to tell the fast
    /// "session" window (≈5h) apart from the "weekly" window (7d). Defaults to
    /// 0 when the field is absent (older API), which forces the legacy
    /// positional mapping.
    #[serde(default)]
    pub(crate) limit_window_seconds: u64,
    pub(crate) reset_after_seconds: u64,
}

/// A rate-limit window whose duration is at least this long is treated as the
/// weekly (7-day) budget; anything shorter is the fast "session" window.
/// Real durations are session ≈ 5h (18000s) and weekly = 7d (604800s), so a
/// 2-day split separates them cleanly and classifies any hypothetical daily
/// (86400s) window as session.
const WEEKLY_WINDOW_MIN_SECS: u64 = 2 * 24 * 60 * 60;

struct CodexWindows<'a> {
    session: Option<&'a CodexWindow>,
    weekly: Option<&'a CodexWindow>,
}

/// Map Codex's `primary`/`secondary` rate-limit windows onto the router's
/// session vs weekly dimensions by their actual `limit_window_seconds`.
///
/// Plus/Team plans return `primary` = 5-hour session window and `secondary` =
/// 7-day weekly window. The Pro plan returns a single 7-day window in
/// `primary_window` with `secondary_window: null`, so a fixed
/// primary=session / secondary=weekly mapping would mislabel Pro's weekly
/// budget as "session" and then treat the absent weekly window as fully
/// consumed — wrongly gating a perfectly healthy Pro account. When window
/// durations are unavailable (older API that omits `limit_window_seconds`),
/// fall back to the legacy positional mapping.
fn classify_codex_windows<'a>(
    primary: &'a CodexWindow,
    secondary: Option<&'a CodexWindow>,
) -> CodexWindows<'a> {
    let durations_known =
        primary.limit_window_seconds > 0 || secondary.is_some_and(|w| w.limit_window_seconds > 0);

    if !durations_known {
        return CodexWindows {
            session: Some(primary),
            weekly: secondary,
        };
    }

    let mut session: Option<&CodexWindow> = None;
    let mut weekly: Option<&CodexWindow> = None;
    for window in std::iter::once(primary).chain(secondary) {
        if window.limit_window_seconds >= WEEKLY_WINDOW_MIN_SECS {
            // Keep the longest weekly-scale window if several qualify.
            if weekly.is_none_or(|cur| window.limit_window_seconds > cur.limit_window_seconds) {
                weekly = Some(window);
            }
        } else if session.is_none_or(|cur| window.limit_window_seconds < cur.limit_window_seconds) {
            // Keep the shortest session-scale window.
            session = Some(window);
        }
    }
    CodexWindows { session, weekly }
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
    /// Whether a weekly budget window was actually observed. `false` means the
    /// provider exposed no weekly window (its usage is unknown, NOT exhausted),
    /// so the selector must treat the account as eligible rather than gating it.
    pub(crate) weekly_known: bool,
    pub(crate) limit_reached: bool,
}

// ── Token refresh ─────────────────────────────────────────────────────

fn apply_claude_token_response(
    creds: &mut serde_json::Value,
    token_resp: &serde_json::Value,
) -> Result<()> {
    let oauth = creds["claudeAiOauth"]
        .as_object_mut()
        .context("claudeAiOauth is not an object")?;

    if let Some(at) = token_resp["access_token"].as_str() {
        oauth.insert("accessToken".into(), serde_json::json!(at));
    }
    if let Some(rt) = token_resp["refresh_token"].as_str() {
        oauth.insert("refreshToken".into(), serde_json::json!(rt));
    }
    let expires_in = token_resp["expires_in"].as_i64().unwrap_or(3600);
    let new_expires_at = chrono::Utc::now().timestamp_millis() + (expires_in * 1000);
    oauth.insert("expiresAt".into(), serde_json::json!(new_expires_at));
    Ok(())
}

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
        anyhow::bail!("{}", claude_refresh_http_error_message(profile_dir, status));
    }

    let token_resp: serde_json::Value = resp
        .json()
        .await
        .context("failed to parse Claude token response")?;

    let mut creds: serde_json::Value = serde_json::from_str(&creds_text)?;
    apply_claude_token_response(&mut creds, &token_resp)?;

    write_0o600_if_unix(&creds_path, serde_json::to_string(&creds)?.as_bytes())?;

    eprintln!("[quota-router] Claude OAuth token refreshed");
    Ok(())
}

async fn refresh_codex_if_needed(profile_dir: &Path) -> Result<()> {
    let auth = load_codex_auth(profile_dir)?;
    let Some(access_token) = auth["tokens"]["access_token"].as_str() else {
        return Ok(());
    };
    let Some(_) = auth["tokens"]["refresh_token"].as_str() else {
        return Ok(());
    };

    if !jwt_expired(access_token) {
        return Ok(());
    }

    eprintln!("[quota-router] refreshing Codex OAuth token via codex CLI...");
    // Codex CLI refresh failures are surfaced from the CLI itself; this path
    // intentionally relies on the CLI's own redaction instead of reprinting
    // raw stderr here.
    refresh_codex_with_cli(profile_dir, &codex_cli_bin())
}

/// Check if a JWT access token is expired (or will expire within the buffer).
fn jwt_expired(token: &str) -> bool {
    let Some(payload) = jwt_payload(token) else {
        return true;
    };
    let Some(exp) = payload["exp"].as_i64() else {
        return true;
    };
    let now = chrono::Utc::now().timestamp();
    now >= exp - REFRESH_BUFFER_SECS
}

fn jwt_payload(token: &str) -> Option<serde_json::Value> {
    let payload_b64 = token.split('.').nth(1)?;
    let Ok(payload_bytes) = general_purpose::URL_SAFE_NO_PAD.decode(payload_b64) else {
        return None;
    };
    serde_json::from_slice::<serde_json::Value>(&payload_bytes).ok()
}

// ── Fetch functions ────────────────────────────────────────────────────

pub(crate) async fn fetch_codex_usage(profile_dir: &Path) -> Result<AccountUsage> {
    refresh_codex_if_needed(profile_dir)
        .await
        .context("codex auth refresh failed")?;

    let auth = load_codex_auth(profile_dir)?;

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

    Ok(codex_usage_from_response(usage))
}

/// Pure mapping from a parsed ChatGPT usage response to the router's unified
/// `AccountUsage`. Separated from the HTTP fetch so the window-classification
/// logic can be unit-tested with mocked rate-limit inputs.
fn codex_usage_from_response(usage: CodexUsageResponse) -> AccountUsage {
    let windows = classify_codex_windows(
        &usage.rate_limit.primary_window,
        usage.rate_limit.secondary_window.as_ref(),
    );

    // Session dimension: when the plan exposes no fast window (e.g. Pro, which
    // only has a weekly cap) there is no separate session budget to spend, so
    // treat it as fully available.
    let (session_used, session_reset) = match windows.session {
        Some(w) => (w.used_percent, w.reset_after_seconds),
        None => (0, 0),
    };

    // Weekly dimension: a PRESENT weekly window carries the real budget. An
    // ABSENT weekly window means we could not observe the weekly usage — mark
    // it unknown (assume available) so the selector never gates the account.
    // A genuinely exhausted weekly window is PRESENT with used_percent≈100, so
    // absent ≠ exhausted.
    let (weekly_used, weekly_reset, weekly_known) = match windows.weekly {
        Some(w) => (w.used_percent, w.reset_after_seconds, true),
        None => (0, 0, false),
    };

    AccountUsage {
        plan: usage.plan_type,
        session_used_pct: session_used,
        session_remaining_pct: 100u32.saturating_sub(session_used),
        session_resets_in_secs: session_reset,
        weekly_used_pct: weekly_used,
        weekly_remaining_pct: 100u32.saturating_sub(weekly_used),
        weekly_resets_in_secs: weekly_reset,
        weekly_known,
        limit_reached: usage.rate_limit.limit_reached,
    }
}

pub(crate) async fn fetch_claude_usage(profile_dir: &Path) -> Result<AccountUsage> {
    if let Err(e) = refresh_claude_if_needed(profile_dir).await {
        eprintln!("[quota-router] {}", sanitize_quota_error_message(&e));
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
        // Claude's usage endpoint always reports the seven-day window.
        weekly_known: true,
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

fn claude_refresh_http_error_message(profile_dir: &Path, status: reqwest::StatusCode) -> String {
    format!(
        "Claude token refresh failed: provider=claude account={} http={status}",
        quota_profile_account_name(profile_dir, Provider::Claude)
    )
}

fn quota_profile_account_name(profile_dir: &Path, provider: Provider) -> String {
    let prefix = format!("{}-", provider.label());
    profile_dir
        .file_name()
        .and_then(|name| name.to_str())
        .map(|name| name.strip_prefix(&prefix).unwrap_or(name).to_string())
        .unwrap_or_else(|| "unknown".to_string())
}

pub(crate) fn sanitize_quota_error_message(err: &anyhow::Error) -> String {
    let chain = format!("{err:#}");
    if quota_error_contains_secret_payload(&chain) {
        return "sensitive auth details redacted".to_string();
    }
    sanitize_provider_error_text(&err.to_string())
}

/// True when a quota/usage error means the account's credentials are revoked or
/// its session ended (HTTP 401, "session has ended", app_session_terminated,
/// "refresh token was revoked"). Such accounts must be EXCLUDED from selection so
/// the router never swaps in a dead profile, which clobbers the live ~/.codex login.
pub(crate) fn is_auth_failure(err: &anyhow::Error) -> bool {
    let chain = format!("{err:#}").to_lowercase();
    chain.contains("401")
        || chain.contains("unauthorized")
        || chain.contains("session has ended")
        || chain.contains("app_session_terminated")
        || chain.contains("refresh token was revoked")
        || chain.contains("could not be refreshed")
}

pub(crate) fn sanitize_provider_error_text(message: &str) -> String {
    if quota_error_contains_secret_payload(message) {
        "sensitive auth details redacted".to_string()
    } else {
        message
            .replace("tokens.access_token", "tokens.token")
            .replace("tokens.refresh_token", "tokens.token")
            .replace("claudeAiOauth.accessToken", "claudeAiOauth.token")
            .replace("claudeAiOauth.refreshToken", "claudeAiOauth.token")
            .replace("access_token", "token")
            .replace("refresh_token", "token")
            .replace("accessToken", "token")
            .replace("refreshToken", "token")
    }
}

fn quota_error_contains_secret_payload(message: &str) -> bool {
    let lower = message.to_ascii_lowercase();
    lower.contains("\"access_token\"")
        || lower.contains("\"refresh_token\"")
        || lower.contains("\"accesstoken\"")
        || lower.contains("\"refreshtoken\"")
        || lower.contains("access_token=")
        || lower.contains("refresh_token=")
        || lower.contains("accesstoken=")
        || lower.contains("refreshtoken=")
        || lower.contains("bearer ")
        || lower.contains("eyj")
}

fn codex_cli_bin() -> PathBuf {
    std::env::var_os("AUTO_QUOTA_CODEX_BIN")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("codex"))
}

fn load_codex_auth(profile_dir: &Path) -> Result<serde_json::Value> {
    let auth_path = codex_home_for_profile(profile_dir).join("auth.json");
    let auth_text = fs::read_to_string(&auth_path)
        .with_context(|| format!("failed to read {}", auth_path.display()))?;
    serde_json::from_str(&auth_text)
        .with_context(|| format!("failed to parse {}", auth_path.display()))
}

fn make_codex_refresh_workspace() -> Result<PathBuf> {
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let dir = std::env::temp_dir().join(format!(
        "auto-quota-codex-refresh-{}-{stamp}",
        std::process::id()
    ));
    fs::create_dir_all(&dir).with_context(|| format!("failed to create {}", dir.display()))?;
    Ok(dir)
}

fn refresh_codex_with_cli(profile_dir: &Path, codex_bin: &Path) -> Result<()> {
    let scratch_dir = make_codex_refresh_workspace()?;
    let mut command = Command::new(codex_bin);
    command
        .arg("exec")
        .arg("--json")
        .arg("--ephemeral")
        .arg("--skip-git-repo-check")
        .arg("--sandbox")
        .arg("read-only")
        .arg("--color")
        .arg("never")
        .arg("--cd")
        .arg(&scratch_dir)
        .arg("-c")
        .arg(format!(
            "model_reasoning_effort=\"{CODEX_REFRESH_REASONING_EFFORT}\""
        ))
        .arg(CODEX_REFRESH_PROMPT)
        .env("CODEX_HOME", codex_home_for_profile(profile_dir))
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .current_dir(&scratch_dir);
    let spawn_result = spawn_retrying_etxtbsy(&mut command)
        .with_context(|| format!("failed to launch Codex at {}", codex_bin.display()));

    let output = match spawn_result {
        Ok(mut child) => {
            let started = Instant::now();
            loop {
                if child
                    .try_wait()
                    .context("failed to poll codex CLI refresh child")?
                    .is_some()
                {
                    break child
                        .wait_with_output()
                        .context("failed to collect codex CLI refresh output")?;
                }
                if started.elapsed() >= CODEX_REFRESH_TIMEOUT {
                    let _ = child.kill();
                    let output = child
                        .wait_with_output()
                        .context("failed to collect timed-out codex CLI refresh output")?;
                    let combined = combined_codex_refresh_output(&output);
                    let _ = fs::remove_dir_all(&scratch_dir);
                    if let Some(message) = summarize_codex_refresh_failure(&combined) {
                        anyhow::bail!("{message}");
                    }
                    anyhow::bail!(
                        "timed out waiting for codex CLI to refresh auth after {}s",
                        CODEX_REFRESH_TIMEOUT.as_secs()
                    );
                }
                thread::sleep(Duration::from_millis(100));
            }
        }
        Err(err) => {
            let _ = fs::remove_dir_all(&scratch_dir);
            return Err(err);
        }
    };
    let _ = fs::remove_dir_all(&scratch_dir);

    let refreshed_auth = load_codex_auth(profile_dir)?;
    let refreshed_access_token = refreshed_auth["tokens"]["access_token"]
        .as_str()
        .context("missing tokens.access_token in refreshed codex auth")?;

    if !jwt_expired(refreshed_access_token) {
        eprintln!("[quota-router] Codex OAuth token refreshed");
        return Ok(());
    }

    let combined_output = combined_codex_refresh_output(&output);
    if let Some(message) = summarize_codex_refresh_failure(&combined_output) {
        anyhow::bail!("{message}");
    }
    if !output.status.success() {
        let combined_output = combined_output.trim();
        if combined_output.is_empty() {
            anyhow::bail!("codex CLI refresh failed with status {}", output.status);
        }
        anyhow::bail!(
            "codex CLI refresh failed with status {}: {combined_output}",
            output.status
        );
    }

    anyhow::bail!("codex CLI exited successfully but left an expired access token");
}

fn combined_codex_refresh_output(output: &std::process::Output) -> String {
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    if stdout.trim().is_empty() {
        stderr.into_owned()
    } else if stderr.trim().is_empty() {
        stdout.into_owned()
    } else {
        format!("{stdout}\n{stderr}")
    }
}

fn summarize_codex_refresh_failure(output: &str) -> Option<String> {
    for line in output.lines() {
        if let Some((_, message)) = line.rsplit_once("Failed to refresh token: ") {
            return Some(message.trim().to_string());
        }
        if let Some((_, message)) = line.rsplit_once(r#""message":"#) {
            return Some(
                message
                    .trim()
                    .trim_matches(|c| c == '"' || c == '}' || c == ',')
                    .replace("\\\"", "\""),
            );
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::os::unix::fs::PermissionsExt;

    fn assert_no_secret_markers(message: &str) {
        for marker in ["access_token", "refresh_token", "Bearer ", "eyJ"] {
            assert!(
                !message.contains(marker),
                "message leaked sensitive marker {marker:?}: {message}"
            );
        }
    }

    fn test_dir(label: &str) -> PathBuf {
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!(
            "auto-quota-usage-test-{label}-{}-{stamp}",
            std::process::id()
        ));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn codex_window(
        used_percent: u32,
        limit_window_seconds: u64,
        reset_after_seconds: u64,
    ) -> CodexWindow {
        CodexWindow {
            used_percent,
            limit_window_seconds,
            reset_after_seconds,
        }
    }

    fn codex_response(
        plan: &str,
        limit_reached: bool,
        primary: CodexWindow,
        secondary: Option<CodexWindow>,
    ) -> CodexUsageResponse {
        CodexUsageResponse {
            plan_type: plan.into(),
            rate_limit: CodexRateLimit {
                limit_reached,
                primary_window: primary,
                secondary_window: secondary,
            },
        }
    }

    #[test]
    fn pro_plan_single_weekly_window_maps_to_weekly_not_session() {
        // Real Pro-plan payload: one 7-day window in `primary_window`, no
        // `secondary_window`. Prior code mislabeled this as "session" and
        // treated the absent weekly window as 100% used, gating the account.
        let usage = codex_usage_from_response(codex_response(
            "pro",
            false,
            codex_window(8, 604800, 574224),
            None,
        ));

        assert!(
            usage.weekly_known,
            "the 7-day primary window IS the weekly budget"
        );
        assert_eq!(usage.weekly_used_pct, 8);
        assert_eq!(usage.weekly_remaining_pct, 92);
        assert_eq!(usage.weekly_resets_in_secs, 574224);
        // No fast window on Pro ⇒ session is fully available, never gated.
        assert_eq!(usage.session_remaining_pct, 100);
        assert!(!usage.limit_reached);
    }

    #[test]
    fn pro_plan_fresh_weekly_window_is_fully_available() {
        let usage = codex_usage_from_response(codex_response(
            "pro",
            false,
            codex_window(0, 604800, 604800),
            None,
        ));
        assert!(usage.weekly_known);
        assert_eq!(usage.weekly_remaining_pct, 100);
        assert_eq!(usage.session_remaining_pct, 100);
    }

    #[test]
    fn plus_plan_maps_primary_to_session_and_secondary_to_weekly() {
        // Plus/Team: 5-hour session window + 7-day weekly window.
        let usage = codex_usage_from_response(codex_response(
            "plus",
            false,
            codex_window(40, 18000, 9000),
            Some(codex_window(70, 604800, 300000)),
        ));

        assert_eq!(usage.session_used_pct, 40);
        assert_eq!(usage.session_remaining_pct, 60);
        assert_eq!(usage.session_resets_in_secs, 9000);
        assert!(usage.weekly_known);
        assert_eq!(usage.weekly_used_pct, 70);
        assert_eq!(usage.weekly_remaining_pct, 30);
        assert_eq!(usage.weekly_resets_in_secs, 300000);
    }

    #[test]
    fn present_weekly_window_at_full_use_is_reported_exhausted_not_unknown() {
        let usage = codex_usage_from_response(codex_response(
            "plus",
            true,
            codex_window(50, 18000, 9000),
            Some(codex_window(100, 604800, 200000)),
        ));
        assert!(
            usage.weekly_known,
            "a real exhausted weekly window is PRESENT"
        );
        assert_eq!(usage.weekly_remaining_pct, 0);
    }

    #[test]
    fn legacy_response_without_durations_falls_back_to_positional_mapping() {
        // Older API omits `limit_window_seconds` (deserializes to 0). Keep the
        // historical primary=session / secondary=weekly mapping.
        let usage = codex_usage_from_response(codex_response(
            "plus",
            false,
            codex_window(30, 0, 9000),
            Some(codex_window(80, 0, 300000)),
        ));
        assert_eq!(usage.session_used_pct, 30);
        assert!(usage.weekly_known);
        assert_eq!(usage.weekly_used_pct, 80);
    }

    #[test]
    fn session_only_response_leaves_weekly_unknown_available() {
        // A response with only a short window and no weekly window: weekly is
        // unknown (assume available), never gated as exhausted.
        let usage = codex_usage_from_response(codex_response(
            "plus",
            false,
            codex_window(60, 18000, 9000),
            None,
        ));
        assert_eq!(usage.session_used_pct, 60);
        assert!(!usage.weekly_known, "no weekly window ⇒ weekly unknown");
        assert_eq!(
            usage.weekly_remaining_pct, 100,
            "unknown weekly assumed available"
        );
    }

    fn fake_jwt(exp: i64) -> String {
        let header = general_purpose::URL_SAFE_NO_PAD.encode(r#"{"alg":"none","typ":"JWT"}"#);
        let payload =
            general_purpose::URL_SAFE_NO_PAD.encode(format!(r#"{{"exp":{exp}}}"#).as_bytes());
        format!("{header}.{payload}.sig")
    }

    fn write_codex_auth(profile_dir: &Path, access_token: &str) {
        let auth = serde_json::json!({
            "auth_mode": "chatgpt",
            "last_refresh": "2026-04-07T17:04:23.095712068Z",
            "tokens": {
                "access_token": access_token,
                "refresh_token": "refresh-token",
                "id_token": "id-token",
                "account_id": "account-id",
            }
        });
        fs::write(
            profile_dir.join("auth.json"),
            serde_json::to_vec_pretty(&auth).unwrap(),
        )
        .unwrap();
    }

    fn write_executable_script(path: &Path, body: &str) {
        fs::write(path, body).unwrap();
        let mut perms = fs::metadata(path).unwrap().permissions();
        perms.set_mode(0o755);
        fs::set_permissions(path, perms).unwrap();
    }

    #[test]
    fn codex_cli_refresh_accepts_updated_auth_even_when_cli_exits_non_zero() {
        let profile_dir = test_dir("refresh-success");
        let bin_dir = test_dir("bin-success");
        let codex_bin = bin_dir.join("codex");
        let future_token = fake_jwt(chrono::Utc::now().timestamp() + 3600);

        write_codex_auth(
            &profile_dir,
            &fake_jwt(chrono::Utc::now().timestamp() - 3600),
        );
        write_executable_script(
            &codex_bin,
            &format!(
                "#!/usr/bin/env bash\ncat > \"$CODEX_HOME/auth.json\" <<'JSON'\n{}\nJSON\nexit 1\n",
                serde_json::to_string(&serde_json::json!({
                    "auth_mode": "chatgpt",
                    "last_refresh": chrono::Utc::now().to_rfc3339(),
                    "tokens": {
                        "access_token": future_token,
                        "refresh_token": "new-refresh-token",
                        "id_token": "new-id-token",
                        "account_id": "account-id",
                    }
                }))
                .unwrap()
            ),
        );

        refresh_codex_with_cli(&profile_dir, &codex_bin).unwrap();

        let auth = load_codex_auth(&profile_dir).unwrap();
        assert_eq!(
            auth["tokens"]["refresh_token"].as_str(),
            Some("new-refresh-token")
        );
    }

    #[test]
    fn codex_cli_refresh_surfaces_human_refresh_error() {
        let profile_dir = test_dir("refresh-error");
        let bin_dir = test_dir("bin-error");
        let codex_bin = bin_dir.join("codex");

        write_codex_auth(
            &profile_dir,
            &fake_jwt(chrono::Utc::now().timestamp() - 3600),
        );
        write_executable_script(
            &codex_bin,
            "#!/usr/bin/env bash\necho '2026-04-17T21:03:08Z ERROR codex_login::auth::manager: Failed to refresh token: Your access token could not be refreshed because your refresh token was already used. Please log out and sign in again.' >&2\nexit 1\n",
        );

        let error = refresh_codex_with_cli(&profile_dir, &codex_bin).unwrap_err();
        assert!(error.to_string().contains("refresh token was already used"));
    }

    #[test]
    fn claude_refresh_error_does_not_leak_body() {
        let profile_dir = PathBuf::from("/tmp/quota-router/profiles/claude-primary");
        let fake_body = r#"{"error":"invalid_grant","access_token":"access_token_value","refresh_token":"refresh_token_value","authorization":"Bearer eyJ.fake.jwt"}"#;

        let sanitized = sanitize_quota_error_message(&anyhow::Error::msg(format!(
            "Claude token refresh returned 401 Unauthorized: {fake_body}"
        )));
        assert!(sanitized.contains("redacted"));
        assert_no_secret_markers(&sanitized);

        let refresh_error =
            claude_refresh_http_error_message(&profile_dir, reqwest::StatusCode::UNAUTHORIZED);
        assert!(refresh_error.contains("provider=claude"));
        assert!(refresh_error.contains("account=primary"));
        assert!(refresh_error.contains("http=401 Unauthorized"));
        assert_no_secret_markers(&refresh_error);
        assert!(!refresh_error.contains(fake_body));
    }

    #[test]
    fn sanitize_quota_error_message_keeps_non_secret_context() {
        let sanitized = sanitize_quota_error_message(&anyhow::Error::msg(
            "missing tokens.access_token in codex auth",
        ));
        assert_eq!(sanitized, "missing tokens.token in codex auth");
        assert_no_secret_markers(&sanitized);
    }

    #[test]
    fn apply_claude_token_response_defaults_expires_in_to_3600_when_absent() {
        let mut creds = serde_json::json!({
            "claudeAiOauth": {
                "accessToken": "old-token",
                "refreshToken": "old-refresh",
                "expiresAt": 1000,
            }
        });
        let token_resp = serde_json::json!({
            "access_token": "new-token",
            "refresh_token": "new-refresh",
            // expires_in intentionally omitted
        });

        apply_claude_token_response(&mut creds, &token_resp).unwrap();

        let oauth = creds["claudeAiOauth"].as_object().unwrap();
        assert_eq!(oauth["accessToken"], "new-token");
        assert_eq!(oauth["refreshToken"], "new-refresh");

        let new_expires_at = oauth["expiresAt"].as_i64().unwrap();
        let now_ms = chrono::Utc::now().timestamp_millis();
        // Should be roughly now + 3600s, within a few seconds tolerance
        let diff_ms = (new_expires_at - (now_ms + 3600 * 1000)).abs();
        assert!(
            diff_ms < 5000,
            "expiresAt should default to ~3600s from now, diff={diff_ms}ms"
        );
    }

    #[test]
    fn apply_claude_token_response_uses_provided_expires_in() {
        let mut creds = serde_json::json!({
            "claudeAiOauth": {
                "accessToken": "old-token",
                "expiresAt": 1000,
            }
        });
        let token_resp = serde_json::json!({
            "access_token": "new-token",
            "expires_in": 7200,
        });

        apply_claude_token_response(&mut creds, &token_resp).unwrap();

        let oauth = creds["claudeAiOauth"].as_object().unwrap();
        let new_expires_at = oauth["expiresAt"].as_i64().unwrap();
        let now_ms = chrono::Utc::now().timestamp_millis();
        let diff_ms = (new_expires_at - (now_ms + 7200 * 1000)).abs();
        assert!(
            diff_ms < 5000,
            "expiresAt should be ~7200s from now, diff={diff_ms}ms"
        );
    }
}
