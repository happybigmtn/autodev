use std::sync::OnceLock;

use regex::Regex;

use crate::quota_config::Provider;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum QuotaVerdict {
    Exhausted,
    OtherError,
    Ok,
}

static CODEX_PATTERNS: OnceLock<Vec<Regex>> = OnceLock::new();
static CLAUDE_PATTERNS: OnceLock<Vec<Regex>> = OnceLock::new();

fn codex_patterns() -> &'static [Regex] {
    CODEX_PATTERNS.get_or_init(|| {
        [
            r"(?i)rate.?limit.?exceeded",
            r"(?i)quota.?exceeded",
            r"(?i)too many requests",
            r"(?i)insufficient.?quota",
            r"(?i)usage.?limit.?reached",
            r"(?i)exceeded.*rate.*limit",
            r"(?i)capacity.*limit",
            r"(?i)billing.*limit",
            r"429",
        ]
        .iter()
        .map(|p| Regex::new(p).expect("invalid regex pattern"))
        .collect()
    })
}

fn claude_patterns() -> &'static [Regex] {
    CLAUDE_PATTERNS.get_or_init(|| {
        [
            r"(?i)rate.?limit.?exceeded",
            r"(?i)quota.?exceeded",
            r"(?i)too many requests",
            r"(?i)overloaded",
            r"(?i)usage.?limit",
            r"(?i)exceeded.*rate.*limit",
            r"(?i)capacity.*limit",
            r"(?i)billing.*limit",
            r"429",
        ]
        .iter()
        .map(|p| Regex::new(p).expect("invalid regex pattern"))
        .collect()
    })
}

/// Scan stderr text for quota-exhaustion signals.
pub(crate) fn check_stderr(provider: Provider, stderr: &str) -> QuotaVerdict {
    if stderr.trim().is_empty() {
        return QuotaVerdict::Ok;
    }

    let patterns = match provider {
        Provider::Codex => codex_patterns(),
        Provider::Claude => claude_patterns(),
    };

    for line in stderr.lines() {
        for pattern in patterns {
            if pattern.is_match(line) {
                return QuotaVerdict::Exhausted;
            }
        }
    }

    // Non-empty stderr that doesn't match quota patterns is an unrelated error
    QuotaVerdict::OtherError
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_codex_rate_limit() {
        let stderr = "Error: rate limit exceeded for this organization";
        assert_eq!(check_stderr(Provider::Codex, stderr), QuotaVerdict::Exhausted);
    }

    #[test]
    fn detects_codex_429() {
        let stderr = "HTTP 429: Too Many Requests";
        assert_eq!(check_stderr(Provider::Codex, stderr), QuotaVerdict::Exhausted);
    }

    #[test]
    fn detects_claude_quota() {
        let stderr = "Error: quota exceeded, please try again later";
        assert_eq!(check_stderr(Provider::Claude, stderr), QuotaVerdict::Exhausted);
    }

    #[test]
    fn generic_error_not_quota() {
        let stderr = "Error: connection refused";
        assert_eq!(check_stderr(Provider::Codex, stderr), QuotaVerdict::OtherError);
    }

    #[test]
    fn empty_stderr_is_ok() {
        assert_eq!(check_stderr(Provider::Codex, ""), QuotaVerdict::Ok);
        assert_eq!(check_stderr(Provider::Codex, "  \n  "), QuotaVerdict::Ok);
    }

    #[test]
    fn partial_match_no_false_positive() {
        let stderr = "Setting rate to unlimited mode";
        // "rate" alone doesn't match "rate.?limit.?exceeded"
        assert_eq!(check_stderr(Provider::Codex, stderr), QuotaVerdict::OtherError);
    }
}
