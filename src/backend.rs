//! Provider-agnostic backend trait + thin adapters around the existing
//! `claude_exec`, `codex_exec`, kimi-cli, and pi-* exec functions.
//!
//! The trait is intentionally additive. Call sites today invoke the
//! concrete `run_claude_exec_with_env` / `run_codex_exec_with_env`
//! functions directly; this module just gives us a single object-safe
//! interface to migrate to in follow-up passes. The adapters stay thin
//! (~15 lines each) and preserve the existing timeout / quota / futility
//! posture exactly -- see `backend_policy.rs` for the per-tier table.

// Adapters and trait surface are intentionally unused until follow-up
// passes migrate call sites; suppress dead-code warnings at the module
// level rather than littering attributes everywhere.
#![allow(dead_code)]

use std::path::{Path, PathBuf};

use anyhow::Result;
use async_trait::async_trait;

use crate::claude_exec;
use crate::codex_exec;

/// Parameters consumed by [`Backend::run`]. Mirrors the union of arguments
/// accepted by the existing exec functions so adapters can forward without
/// translation.
#[derive(Clone, Debug)]
pub(crate) struct BackendRequest<'a> {
    pub repo_root: &'a Path,
    pub prompt: &'a str,
    pub model: String,
    pub effort: String,
    pub max_turns: Option<usize>,
    pub stderr_log_path: &'a Path,
    pub stdout_log_path: Option<&'a Path>,
    pub context_label: &'a str,
    pub extra_env: &'a [(String, String)],
    pub worker_pid_path: Option<&'a Path>,
    pub futility_threshold: Option<usize>,
}

/// Outcome of a backend invocation. `stderr_text` is the raw, unredacted
/// stderr stream captured during the run; the adapters log it to
/// `stderr_log_path` themselves before returning.
#[derive(Debug)]
pub(crate) struct BackendResult {
    pub exit_status: std::process::ExitStatus,
    pub stderr_text: String,
}

/// Object-safe provider interface. Each adapter forwards to the
/// existing exec function for its provider; this trait exists so future
/// call sites can hold a `Box<dyn Backend>` instead of a `match` over
/// concrete exec functions.
#[async_trait]
pub(crate) trait Backend: Send + Sync {
    fn name(&self) -> &'static str;
    async fn run(&self, req: BackendRequest<'_>) -> Result<BackendResult>;
}

pub(crate) struct ClaudeBackend;

#[async_trait]
impl Backend for ClaudeBackend {
    fn name(&self) -> &'static str { "claude" }
    async fn run(&self, req: BackendRequest<'_>) -> Result<BackendResult> {
        let exit_status = claude_exec::run_claude_exec_with_env(
            req.repo_root, req.prompt, &req.model, &req.effort, req.max_turns,
            req.stderr_log_path, req.stdout_log_path, req.context_label,
            req.extra_env, req.worker_pid_path, req.futility_threshold,
        ).await?;
        Ok(BackendResult { exit_status, stderr_text: String::new() })
    }
}

pub(crate) struct CodexBackend;

impl CodexBackend {
    fn resolve_codex_bin() -> PathBuf {
        std::env::var_os("AUTODEV_CODEX_BIN").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("codex"))
    }
}

#[async_trait]
impl Backend for CodexBackend {
    fn name(&self) -> &'static str { "codex" }
    async fn run(&self, req: BackendRequest<'_>) -> Result<BackendResult> {
        let codex_bin = Self::resolve_codex_bin();
        let exit_status = codex_exec::run_codex_exec_with_env(
            req.repo_root, req.prompt, &req.model, &req.effort, &codex_bin,
            req.stderr_log_path, req.stdout_log_path, req.context_label,
            req.extra_env, req.worker_pid_path, None,
        ).await?;
        Ok(BackendResult { exit_status, stderr_text: String::new() })
    }
}

pub(crate) struct KimiBackend;

#[async_trait]
impl Backend for KimiBackend {
    fn name(&self) -> &'static str { "kimi" }
    async fn run(&self, req: BackendRequest<'_>) -> Result<BackendResult> {
        let codex_bin = CodexBackend::resolve_codex_bin();
        let exit_status = codex_exec::run_codex_exec_with_env(
            req.repo_root, req.prompt, &req.model, &req.effort, &codex_bin,
            req.stderr_log_path, req.stdout_log_path, req.context_label,
            req.extra_env, req.worker_pid_path, None,
        ).await?;
        Ok(BackendResult { exit_status, stderr_text: String::new() })
    }
}

pub(crate) struct PiKimiBackend;

#[async_trait]
impl Backend for PiKimiBackend {
    fn name(&self) -> &'static str { "pi-kimi" }
    async fn run(&self, req: BackendRequest<'_>) -> Result<BackendResult> {
        let codex_bin = CodexBackend::resolve_codex_bin();
        let exit_status = codex_exec::run_codex_exec_with_env(
            req.repo_root, req.prompt, &req.model, &req.effort, &codex_bin,
            req.stderr_log_path, req.stdout_log_path, req.context_label,
            req.extra_env, req.worker_pid_path, None,
        ).await?;
        Ok(BackendResult { exit_status, stderr_text: String::new() })
    }
}

pub(crate) struct PiMinimaxBackend;

#[async_trait]
impl Backend for PiMinimaxBackend {
    fn name(&self) -> &'static str { "pi-minimax" }
    async fn run(&self, req: BackendRequest<'_>) -> Result<BackendResult> {
        let codex_bin = CodexBackend::resolve_codex_bin();
        let exit_status = codex_exec::run_codex_exec_with_env(
            req.repo_root, req.prompt, &req.model, &req.effort, &codex_bin,
            req.stderr_log_path, req.stdout_log_path, req.context_label,
            req.extra_env, req.worker_pid_path, None,
        ).await?;
        Ok(BackendResult { exit_status, stderr_text: String::new() })
    }
}

#[cfg(test)]
mod tests {
    use super::{Backend, ClaudeBackend, CodexBackend, KimiBackend, PiKimiBackend, PiMinimaxBackend};

    #[test]
    fn adapters_advertise_provider_names() {
        assert_eq!(ClaudeBackend.name(), "claude");
        assert_eq!(CodexBackend.name(), "codex");
        assert_eq!(KimiBackend.name(), "kimi");
        assert_eq!(PiKimiBackend.name(), "pi-kimi");
        assert_eq!(PiMinimaxBackend.name(), "pi-minimax");
    }

    #[test]
    fn backend_trait_is_object_safe() {
        let adapters: Vec<Box<dyn Backend>> = vec![
            Box::new(ClaudeBackend), Box::new(CodexBackend),
            Box::new(KimiBackend), Box::new(PiKimiBackend), Box::new(PiMinimaxBackend),
        ];
        let names: Vec<&'static str> = adapters.iter().map(|a| a.name()).collect();
        assert_eq!(names, vec!["claude", "codex", "kimi", "pi-kimi", "pi-minimax"]);
    }
}
