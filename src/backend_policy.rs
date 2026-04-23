#![allow(dead_code)]

use serde::Serialize;

use crate::codex_exec::MAX_CODEX_MODEL_CONTEXT_WINDOW;
use crate::kimi_backend::KIMI_CLI_DEFAULT_MODEL;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum QuotaRouting {
    SharedWrapperWhenConfigured,
    ManualQuotaOpenWhenConfigured,
    AlwaysQuotaOpen,
    None,
    InheritedFromCallerArgs,
    NotApplicable,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum OutputMode {
    CodexJson,
    ClaudeStreamJson,
    KimiStreamJson,
    PiJson,
    PlainText,
    Inherited,
    ExternalRuntimeLogs,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ContextWindowPolicy {
    NotSet,
    CallerOptional,
    MaxCodexWindow,
    RenderedByExternalRuntime,
    NotApplicable,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
pub(crate) struct BackendPolicy {
    pub(crate) policy_id: &'static str,
    pub(crate) surface: &'static str,
    pub(crate) provider_name: &'static str,
    pub(crate) command_family: &'static str,
    pub(crate) model: Option<&'static str>,
    pub(crate) effort: Option<&'static str>,
    pub(crate) quota_routing: QuotaRouting,
    pub(crate) dangerous_flags: &'static [&'static str],
    pub(crate) output_mode: OutputMode,
    pub(crate) logging_posture: &'static str,
    pub(crate) timeout_posture: &'static str,
    pub(crate) futility_posture: &'static str,
    pub(crate) context_window: ContextWindowPolicy,
    pub(crate) context_window_tokens: Option<i64>,
}

pub(crate) const KNOWN_BACKEND_POLICIES: &[BackendPolicy] = &[
    BackendPolicy {
        policy_id: "shared_codex_exec",
        surface: "src/codex_exec.rs",
        provider_name: "codex",
        command_family: "codex exec",
        model: Some("caller-supplied"),
        effort: Some("caller-supplied"),
        quota_routing: QuotaRouting::SharedWrapperWhenConfigured,
        dangerous_flags: &[
            "--dangerously-bypass-approvals-and-sandbox",
            "--skip-git-repo-check",
        ],
        output_mode: OutputMode::CodexJson,
        logging_posture: "streams codex json and can mirror stdout/stderr to caller logs",
        timeout_posture: "no internal timeout; caller owns retry or timeout",
        futility_posture: "none",
        context_window: ContextWindowPolicy::CallerOptional,
        context_window_tokens: Some(MAX_CODEX_MODEL_CONTEXT_WINDOW),
    },
    BackendPolicy {
        policy_id: "shared_claude_exec",
        surface: "src/claude_exec.rs",
        provider_name: "claude",
        command_family: "claude -p",
        model: Some("caller-supplied; non-claude or empty resolves to opus"),
        effort: Some("caller-supplied; empty resolves to high"),
        quota_routing: QuotaRouting::SharedWrapperWhenConfigured,
        dangerous_flags: &["--dangerously-skip-permissions"],
        output_mode: OutputMode::ClaudeStreamJson,
        logging_posture: "streams claude stream-json and can mirror stdout/stderr to caller logs",
        timeout_posture: "no wall-clock timeout",
        futility_posture: "kills after consecutive empty tool results; synthetic exit 137",
        context_window: ContextWindowPolicy::NotSet,
        context_window_tokens: None,
    },
    BackendPolicy {
        policy_id: "generation_direct_claude",
        surface: "src/generation.rs",
        provider_name: "claude",
        command_family: "claude -p",
        model: Some("generation author model"),
        effort: Some("generation reasoning effort"),
        quota_routing: QuotaRouting::None,
        dangerous_flags: &["--dangerously-skip-permissions"],
        output_mode: OutputMode::PlainText,
        logging_posture: "writes prompt log and response file when stdout is non-empty",
        timeout_posture: "bounded only by --max-turns; no wall-clock timeout",
        futility_posture: "none",
        context_window: ContextWindowPolicy::NotSet,
        context_window_tokens: None,
    },
    BackendPolicy {
        policy_id: "direct_codex_max_context",
        surface: "src/bug_command.rs, src/nemesis.rs, src/audit_command.rs",
        provider_name: "codex",
        command_family: "codex exec",
        model: Some("pipeline phase model"),
        effort: Some("pipeline phase reasoning effort"),
        quota_routing: QuotaRouting::None,
        dangerous_flags: &[
            "--dangerously-bypass-approvals-and-sandbox",
            "--skip-git-repo-check",
        ],
        output_mode: OutputMode::CodexJson,
        logging_posture: "pipeline-owned stdout parsing and stderr capture",
        timeout_posture: "pipeline-owned; bug and audit use phase timeouts, nemesis has none",
        futility_posture: "none; nemesis may fall back to codex after non-codex failures",
        context_window: ContextWindowPolicy::MaxCodexWindow,
        context_window_tokens: Some(MAX_CODEX_MODEL_CONTEXT_WINDOW),
    },
    BackendPolicy {
        policy_id: "manual_quota_codex_planner",
        surface: "src/symphony_command.rs",
        provider_name: "codex",
        command_family: "auto quota open codex exec or codex exec",
        model: Some("planner model"),
        effort: Some("planner reasoning effort"),
        quota_routing: QuotaRouting::ManualQuotaOpenWhenConfigured,
        dangerous_flags: &[
            "--dangerously-bypass-approvals-and-sandbox",
            "--skip-git-repo-check",
        ],
        output_mode: OutputMode::CodexJson,
        logging_posture: "captures planner stdout/stderr into strings with heartbeat",
        timeout_posture: "no internal timeout",
        futility_posture: "none",
        context_window: ContextWindowPolicy::NotSet,
        context_window_tokens: None,
    },
    BackendPolicy {
        policy_id: "rendered_quota_codex_worker",
        surface: "src/symphony_command.rs",
        provider_name: "codex",
        command_family: "auto quota open codex app-server",
        model: Some("rendered symphony worker model"),
        effort: Some("rendered symphony worker reasoning effort"),
        quota_routing: QuotaRouting::AlwaysQuotaOpen,
        dangerous_flags: &["rendered approval and sandbox policy"],
        output_mode: OutputMode::Inherited,
        logging_posture: "output handling is owned by the external symphony runtime",
        timeout_posture: "workflow renders read and wall-clock limits",
        futility_posture: "external runtime owned",
        context_window: ContextWindowPolicy::RenderedByExternalRuntime,
        context_window_tokens: None,
    },
    BackendPolicy {
        policy_id: "kimi_cli",
        surface: "src/kimi_backend.rs",
        provider_name: "kimi-cli",
        command_family: "kimi-cli",
        model: Some(KIMI_CLI_DEFAULT_MODEL),
        effort: Some("mapped to --thinking or --no-thinking"),
        quota_routing: QuotaRouting::None,
        dangerous_flags: &["--yolo"],
        output_mode: OutputMode::KimiStreamJson,
        logging_posture: "caller streams stdout with heartbeat and appends stderr to pipeline logs",
        timeout_posture: "caller-owned; bug and audit use phase timeouts, nemesis has none",
        futility_posture: "caller-owned fallback where implemented",
        context_window: ContextWindowPolicy::NotSet,
        context_window_tokens: None,
    },
    BackendPolicy {
        policy_id: "pi_kimi",
        surface: "src/pi_backend.rs",
        provider_name: "pi-kimi",
        command_family: "pi",
        model: Some("kimi-coding/k2p6"),
        effort: Some("mapped to --thinking"),
        quota_routing: QuotaRouting::None,
        dangerous_flags: &["--tools read,bash,edit,write,grep,find,ls"],
        output_mode: OutputMode::PiJson,
        logging_posture: "caller streams stdout with heartbeat and appends stderr to pipeline logs",
        timeout_posture: "caller-owned; bug uses phase timeouts, nemesis has none",
        futility_posture: "nemesis falls back to codex on PI failure",
        context_window: ContextWindowPolicy::NotSet,
        context_window_tokens: None,
    },
    BackendPolicy {
        policy_id: "pi_minimax",
        surface: "src/pi_backend.rs",
        provider_name: "pi-minimax",
        command_family: "pi",
        model: Some("minimax/MiniMax-M2.7-highspeed"),
        effort: Some("mapped to --thinking"),
        quota_routing: QuotaRouting::None,
        dangerous_flags: &["--tools read,bash,edit,write,grep,find,ls"],
        output_mode: OutputMode::PiJson,
        logging_posture: "caller streams stdout with heartbeat and appends stderr to pipeline logs",
        timeout_posture: "caller-owned; bug uses phase timeouts, nemesis has none",
        futility_posture: "nemesis falls back to codex on PI failure",
        context_window: ContextWindowPolicy::NotSet,
        context_window_tokens: None,
    },
    BackendPolicy {
        policy_id: "quota_open",
        surface: "src/quota_exec.rs",
        provider_name: "quota-open",
        command_family: "auto quota open",
        model: Some("inherited from provider argv"),
        effort: Some("inherited from provider argv"),
        quota_routing: QuotaRouting::InheritedFromCallerArgs,
        dangerous_flags: &["inherited from provider argv"],
        output_mode: OutputMode::Inherited,
        logging_posture: "inherits provider stdout/stderr and prints selected account to stderr",
        timeout_posture: "no internal timeout",
        futility_posture: "provider/caller owned",
        context_window: ContextWindowPolicy::NotApplicable,
        context_window_tokens: None,
    },
    BackendPolicy {
        policy_id: "symphony_runtime",
        surface: "src/symphony_command.rs",
        provider_name: "symphony-runtime",
        command_family: "symphony",
        model: None,
        effort: None,
        quota_routing: QuotaRouting::NotApplicable,
        dangerous_flags: &["--i-understand-that-this-will-be-running-without-the-usual-guardrails"],
        output_mode: OutputMode::ExternalRuntimeLogs,
        logging_posture: "rust process inherits stdout/stderr and points to symphony log root",
        timeout_posture: "waits for process exit; no rust-side timeout",
        futility_posture: "external runtime owned",
        context_window: ContextWindowPolicy::NotApplicable,
        context_window_tokens: None,
    },
    BackendPolicy {
        policy_id: "supporting_git",
        surface: "src/util.rs and command modules",
        provider_name: "git",
        command_family: "git",
        model: None,
        effort: None,
        quota_routing: QuotaRouting::NotApplicable,
        dangerous_flags: &[],
        output_mode: OutputMode::PlainText,
        logging_posture: "caller-owned repository state and diff output",
        timeout_posture: "caller-owned",
        futility_posture: "not applicable",
        context_window: ContextWindowPolicy::NotApplicable,
        context_window_tokens: None,
    },
    BackendPolicy {
        policy_id: "supporting_parallel_host_processes",
        surface: "src/parallel_command.rs",
        provider_name: "parallel-host-process",
        command_family: "sh, agent-browser, tmux, kill",
        model: None,
        effort: None,
        quota_routing: QuotaRouting::NotApplicable,
        dangerous_flags: &[],
        output_mode: OutputMode::PlainText,
        logging_posture: "host preflight and terminal management output is caller-owned",
        timeout_posture: "caller-owned",
        futility_posture: "not applicable",
        context_window: ContextWindowPolicy::NotApplicable,
        context_window_tokens: None,
    },
];

pub(crate) fn known_backend_policies() -> &'static [BackendPolicy] {
    KNOWN_BACKEND_POLICIES
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use serde_json::{json, Value};

    use super::*;
    use crate::pi_backend::PiProvider;

    #[test]
    fn serializes_known_backend_policy_inventory() {
        let serialized = serde_json::to_value(known_backend_policies()).unwrap();
        let policies = serialized.as_array().unwrap();
        assert_eq!(policies.len(), KNOWN_BACKEND_POLICIES.len());
        assert!(policies.len() >= 13);

        let ids = policies
            .iter()
            .map(|policy| policy["policy_id"].as_str().unwrap())
            .collect::<BTreeSet<_>>();
        assert_eq!(ids.len(), policies.len());
        for expected in [
            "shared_codex_exec",
            "shared_claude_exec",
            "generation_direct_claude",
            "direct_codex_max_context",
            "manual_quota_codex_planner",
            "rendered_quota_codex_worker",
            "kimi_cli",
            "pi_kimi",
            "pi_minimax",
            "quota_open",
            "symphony_runtime",
            "supporting_git",
            "supporting_parallel_host_processes",
        ] {
            assert!(ids.contains(expected), "missing policy fixture {expected}");
        }

        let providers = policies
            .iter()
            .map(|policy| policy["provider_name"].as_str().unwrap())
            .collect::<BTreeSet<_>>();
        for expected in [
            "claude",
            "codex",
            "kimi-cli",
            "pi-kimi",
            "pi-minimax",
            "quota-open",
            "symphony-runtime",
            "git",
            "parallel-host-process",
        ] {
            assert!(providers.contains(expected), "missing provider {expected}");
        }

        for policy in policies {
            for required in [
                "provider_name",
                "model",
                "effort",
                "quota_routing",
                "dangerous_flags",
                "output_mode",
                "logging_posture",
                "timeout_posture",
                "futility_posture",
                "context_window",
                "context_window_tokens",
            ] {
                assert!(
                    policy.get(required).is_some(),
                    "missing {required} in {}",
                    policy["policy_id"]
                );
            }
        }

        assert_eq!(
            policy_by_id(policies, "shared_codex_exec"),
            json!({
                "policy_id": "shared_codex_exec",
                "surface": "src/codex_exec.rs",
                "provider_name": "codex",
                "command_family": "codex exec",
                "model": "caller-supplied",
                "effort": "caller-supplied",
                "quota_routing": "shared_wrapper_when_configured",
                "dangerous_flags": [
                    "--dangerously-bypass-approvals-and-sandbox",
                    "--skip-git-repo-check"
                ],
                "output_mode": "codex_json",
                "logging_posture": "streams codex json and can mirror stdout/stderr to caller logs",
                "timeout_posture": "no internal timeout; caller owns retry or timeout",
                "futility_posture": "none",
                "context_window": "caller_optional",
                "context_window_tokens": MAX_CODEX_MODEL_CONTEXT_WINDOW
            })
        );

        assert_eq!(
            policy_by_id(policies, "kimi_cli")["model"],
            json!(KIMI_CLI_DEFAULT_MODEL)
        );
        assert_eq!(
            policy_by_id(policies, "pi_kimi")["model"],
            json!(PiProvider::Kimi.default_model())
        );
        assert_eq!(
            policy_by_id(policies, "pi_minimax")["model"],
            json!(PiProvider::Minimax.default_model())
        );
        assert_eq!(
            policy_by_id(policies, "manual_quota_codex_planner")["quota_routing"],
            json!("manual_quota_open_when_configured")
        );
        assert_eq!(
            policy_by_id(policies, "rendered_quota_codex_worker")["quota_routing"],
            json!("always_quota_open")
        );
    }

    fn policy_by_id(policies: &[Value], id: &str) -> Value {
        policies
            .iter()
            .find(|policy| policy["policy_id"] == id)
            .unwrap_or_else(|| panic!("missing policy {id}"))
            .clone()
    }
}
