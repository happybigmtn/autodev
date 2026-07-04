//! Verification-receipt model, footer codec, freshness checks, and inspection.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result};
use base64::Engine;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use shlex::split as shell_split;

use crate::completion_artifacts::artifacts::{
    current_declared_artifact_hashes, declared_artifact_path, declared_completion_artifacts,
    sha256_hex,
};
use crate::completion_artifacts::audit::unresolved_owned_audit_findings;
use crate::completion_artifacts::review_contains_task;
use crate::completion_artifacts::verification::verification_plan;

const RECEIPT_FOOTER_VERSION: &str = "Auto-Verification-Receipt-Version:";
const RECEIPT_FOOTER_TASK: &str = "Auto-Verification-Receipt-Task:";
const RECEIPT_FOOTER_JSON: &str = "Auto-Verification-Receipt-JSON:";

pub(crate) fn verification_receipt_path(repo_root: &Path, task_id: &str) -> PathBuf {
    verification_receipt_root(repo_root).join(format!("{task_id}.json"))
}

pub(crate) fn verification_receipt_commit_footer(
    repo_root: &Path,
    task_id: &str,
) -> Result<Option<String>> {
    let path = verification_receipt_path(repo_root, task_id);
    if !path.exists() {
        return Ok(None);
    }
    let receipt_text =
        fs::read_to_string(&path).with_context(|| format!("failed to read {}", path.display()))?;
    let compact = compact_receipt_json_for_footer(&receipt_text)
        .with_context(|| format!("failed to prepare receipt footer from {}", path.display()))?;
    let encoded = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(compact.as_bytes());
    Ok(Some(format!(
        "{RECEIPT_FOOTER_VERSION} 1\n{RECEIPT_FOOTER_TASK} {task_id}\n{RECEIPT_FOOTER_JSON} {encoded}"
    )))
}

pub(crate) fn legacy_verification_receipt_backfill_footer(
    repo_root: &Path,
    task_id: &str,
    task_markdown: &str,
) -> Result<Option<String>> {
    let verification = verification_plan(task_markdown);
    if verification.executable_commands.is_empty() {
        return Ok(None);
    }

    let review_text = fs::read_to_string(repo_root.join("REVIEW.md")).unwrap_or_default();
    if !review_contains_task(&review_text, task_id) {
        return Ok(None);
    }

    let declared_artifacts = declared_completion_artifacts(task_markdown);
    if declared_artifacts
        .iter()
        .any(|relative| declared_artifact_path(repo_root, relative).is_none())
    {
        return Ok(None);
    }
    if !unresolved_owned_audit_findings(repo_root, task_id, task_markdown).is_empty() {
        return Ok(None);
    }

    let receipt_path = verification_receipt_path(repo_root, task_id);
    let receipt_text = match fs::read_to_string(&receipt_path) {
        Ok(text) => text,
        Err(_) => return Ok(None),
    };
    let receipt = match serde_json::from_str::<VerificationReceipt>(&receipt_text) {
        Ok(receipt) => receipt,
        Err(_) => return Ok(None),
    };

    if verification_receipt_content_problem(
        &receipt_path,
        &receipt,
        &verification.executable_commands,
    )
    .is_some()
    {
        return Ok(None);
    }

    for (path, current_hash) in
        current_declared_artifact_hashes(repo_root, &receipt_path, &declared_artifacts)
    {
        let matches = receipt
            .declared_artifacts
            .iter()
            .find(|artifact| artifact.path == path)
            .and_then(|artifact| artifact.sha256.as_deref())
            .is_some_and(|recorded| recorded == current_hash);
        if !matches {
            return Ok(None);
        }
    }

    verification_receipt_commit_footer(repo_root, task_id)
}

pub(crate) fn latest_verification_receipt_footer(
    repo_root: &Path,
    task_id: &str,
) -> Option<VerificationReceiptFooter> {
    git_verification_receipt_footers(repo_root)
        .into_iter()
        .find(|footer| footer.task_id == task_id)
}

pub(crate) fn git_verification_receipt_footers(repo_root: &Path) -> Vec<VerificationReceiptFooter> {
    let Ok(output) = Command::new("git")
        .arg("-C")
        .arg(repo_root)
        .args([
            "log",
            "--format=%H%x1f%B%x1e",
            "--grep=Auto-Verification-Receipt-Task:",
            "HEAD",
        ])
        .output()
    else {
        return Vec::new();
    };
    if !output.status.success() {
        return Vec::new();
    }
    let rendered = String::from_utf8_lossy(&output.stdout);
    rendered
        .split('\x1e')
        .filter_map(|record| {
            let (commit, body) = record.split_once('\x1f')?;
            parse_verification_receipt_footer(commit.trim(), body)
        })
        .collect()
}

pub(crate) fn shared_footer_receipt_freshness_problem(
    repo_root: &Path,
    footer: &VerificationReceiptFooter,
    expected_commands: &[String],
    declared_artifacts: &[String],
) -> Result<Option<String>> {
    let receipt =
        serde_json::from_str::<VerificationReceipt>(&footer.receipt_text).with_context(|| {
            format!(
                "invalid verification receipt footer for `{}` in commit {}",
                footer.task_id, footer.commit
            )
        })?;
    Ok(verification_receipt_freshness_problem_for_source(
        repo_root,
        &PathBuf::from(format!(
            "commit:{}:Auto-Verification-Receipt",
            footer.commit
        )),
        &receipt,
        expected_commands,
        declared_artifacts,
        VerificationReceiptSource::CommitFooter,
    ))
}

fn parse_verification_receipt_footer(
    commit: &str,
    body: &str,
) -> Option<VerificationReceiptFooter> {
    let mut task_id = None::<String>;
    let mut encoded = None::<String>;
    let mut version_ok = false;
    for line in body.lines() {
        let trimmed = line.trim();
        if let Some(value) = trimmed.strip_prefix(RECEIPT_FOOTER_VERSION) {
            version_ok = value.trim() == "1";
        } else if let Some(value) = trimmed.strip_prefix(RECEIPT_FOOTER_TASK) {
            let value = value.trim();
            if !value.is_empty() {
                task_id = Some(value.to_string());
            }
        } else if let Some(value) = trimmed.strip_prefix(RECEIPT_FOOTER_JSON) {
            let value = value.trim();
            if !value.is_empty() {
                encoded = Some(value.to_string());
            }
        }
    }
    if !version_ok {
        return None;
    }
    let task_id = task_id?;
    let encoded = encoded?;
    let decoded = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(encoded.as_bytes())
        .ok()?;
    let receipt_text = String::from_utf8(decoded).ok()?;
    Some(VerificationReceiptFooter {
        task_id,
        commit: commit.to_string(),
        receipt_text,
    })
}

fn compact_receipt_json_for_footer(receipt_text: &str) -> Result<String> {
    let mut value = serde_json::from_str::<Value>(receipt_text)?;
    prune_receipt_output_tails(&mut value);
    Ok(serde_json::to_string(&value)?)
}

fn prune_receipt_output_tails(value: &mut Value) {
    let Some(commands) = value.get_mut("commands").and_then(Value::as_array_mut) else {
        return;
    };
    for command in commands {
        let Some(output) = command
            .get_mut("output_summary")
            .and_then(Value::as_object_mut)
        else {
            continue;
        };
        output.remove("stdout_tail");
        output.remove("stderr_tail");
    }
}

pub(crate) fn verification_receipt_root(repo_root: &Path) -> PathBuf {
    if repo_root.file_name().and_then(|name| name.to_str()) == Some("repo") {
        let ancestors = repo_root.ancestors().collect::<Vec<_>>();
        if ancestors
            .iter()
            .any(|ancestor| ancestor.file_name().and_then(|name| name.to_str()) == Some("lanes"))
        {
            if let Some(auto_root) = ancestors.iter().find(|ancestor| {
                ancestor.file_name().and_then(|name| name.to_str()) == Some(".auto")
            }) {
                return auto_root.join("symphony/verification-receipts");
            }
        }
    }

    repo_root.join(".auto/symphony/verification-receipts")
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, Eq, PartialEq)]
struct VerificationReceipt {
    #[serde(default)]
    task_id: Option<String>,
    #[serde(default)]
    commit: Option<String>,
    #[serde(default)]
    dirty_state: Option<VerificationDirtyState>,
    #[serde(default)]
    plan_hash: Option<String>,
    #[serde(default, alias = "completion_artifacts")]
    declared_artifacts: Vec<VerificationReceiptArtifact>,
    #[serde(default)]
    commands: Vec<VerificationReceiptCommand>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, Eq, PartialEq)]
struct VerificationDirtyState {
    #[serde(default)]
    fingerprint: Option<String>,
    #[serde(default)]
    entries: Vec<Value>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, Eq, PartialEq)]
struct VerificationReceiptArtifact {
    path: String,
    #[serde(default)]
    sha256: Option<String>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, Eq, PartialEq)]
struct VerificationReceiptCommand {
    command: String,
    #[serde(default)]
    argv: Vec<String>,
    #[serde(default)]
    supersedes: Vec<String>,
    #[serde(default)]
    expected_argv: Option<Vec<String>>,
    #[serde(default, alias = "exit_status")]
    exit_code: Option<i32>,
    #[serde(default)]
    status: Option<String>,
    #[serde(default)]
    runner_summary: Option<VerificationRunnerSummary>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, Eq, PartialEq)]
struct VerificationRunnerSummary {
    #[serde(default)]
    zero_test_detected: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum VerificationReceiptSource {
    JsonFile,
    CommitFooter,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct VerificationReceiptFooter {
    pub(crate) task_id: String,
    pub(crate) commit: String,
    pub(crate) receipt_text: String,
}

pub(crate) fn inspect_verification_receipt(
    repo_root: &Path,
    verification_receipt_required: bool,
    verification_wrapper_present: bool,
    verification_receipt_path: &Path,
    expected_commands: &[String],
    declared_artifacts: &[String],
) -> (bool, Option<String>) {
    if !verification_receipt_required {
        return (true, None);
    }
    // Try the committed footer first — it's durable proof embedded in a
    // closeout commit message. If the footer is fresh and content-clean,
    // accept it. If the footer is stale or has a content problem, fall
    // through to check the on-disk JSON receipt before declaring failure;
    // a fresh file should always supersede a stale footer (a previous run
    // may have written a footer pre-dating new wrapper output or a plan
    // refresh).
    if let Some(footer) = latest_verification_receipt_footer(
        repo_root,
        task_id_from_receipt_path(verification_receipt_path)
            .as_deref()
            .unwrap_or_default(),
    ) {
        let footer_path = PathBuf::from(format!(
            "commit:{}:Auto-Verification-Receipt",
            footer.commit
        ));
        match serde_json::from_str::<VerificationReceipt>(&footer.receipt_text) {
            Ok(receipt) => {
                let footer_freshness = verification_receipt_freshness_problem_for_source(
                    repo_root,
                    &footer_path,
                    &receipt,
                    expected_commands,
                    declared_artifacts,
                    VerificationReceiptSource::CommitFooter,
                );
                let footer_content = if footer_freshness.is_none() {
                    verification_receipt_content_problem(&footer_path, &receipt, expected_commands)
                } else {
                    None
                };
                if footer_freshness.is_none() && footer_content.is_none() {
                    return (true, None);
                }
                // Footer is stale or content-incomplete; do NOT return here
                // — fall through to the on-disk JSON receipt path below.
                // If the file path also fails, we'll return a combined
                // error mentioning the footer for context.
            }
            Err(err) => {
                // Malformed footer — surface as error but still try the file.
                eprintln!(
                    "warning: invalid verification receipt footer for `{}` in commit {}: {err}; falling back to on-disk receipt",
                    footer.task_id, footer.commit
                );
            }
        }
    }
    if !verification_wrapper_present {
        return (
            false,
            Some(format!(
                "missing scripts/run-task-verification.sh; executable Verification command(s) need receipt-backed proof: {}",
                expected_commands
                    .iter()
                    .map(|command| format!("`{command}`"))
                    .collect::<Vec<_>>()
                    .join(", ")
            )),
        );
    }
    if !verification_receipt_path.exists() {
        return (false, None);
    }

    let receipt_text = match fs::read_to_string(verification_receipt_path) {
        Ok(text) => text,
        Err(err) => {
            return (
                false,
                Some(format!(
                    "failed to read verification receipt `{}`: {err}",
                    verification_receipt_path.display()
                )),
            );
        }
    };
    let receipt = match serde_json::from_str::<VerificationReceipt>(&receipt_text) {
        Ok(receipt) => receipt,
        Err(err) => {
            return (
                false,
                Some(format!(
                    "invalid verification receipt `{}`: {err}",
                    verification_receipt_path.display()
                )),
            );
        }
    };

    if let Some(problem) = verification_receipt_freshness_problem(
        repo_root,
        verification_receipt_path,
        &receipt,
        expected_commands,
        declared_artifacts,
    ) {
        return (
            false,
            Some(format!(
                "stale verification receipt `{}`: {problem}",
                verification_receipt_path.display()
            )),
        );
    }

    let mut missing = expected_commands
        .iter()
        .filter(|command| {
            !receipt
                .commands
                .iter()
                .any(|entry| verification_receipt_command_matches(entry, command))
        })
        .cloned()
        .collect::<Vec<_>>();
    missing.sort();
    if !missing.is_empty() {
        return (
            false,
            Some(format!(
                "verification receipt `{}` is missing command(s): {}",
                verification_receipt_path.display(),
                missing
                    .iter()
                    .map(|command| format!("`{command}`"))
                    .collect::<Vec<_>>()
                    .join(", ")
            )),
        );
    }

    let mut failed = expected_commands
        .iter()
        .filter(|command| {
            let matching_entries = receipt
                .commands
                .iter()
                .filter(|entry| verification_receipt_command_matches(entry, command))
                .collect::<Vec<_>>();
            !matching_entries.is_empty()
                && matching_entries.iter().all(|entry| {
                    entry.status.as_deref() != Some("passed") || entry.exit_code != Some(0)
                })
        })
        .cloned()
        .collect::<Vec<_>>();
    failed.sort();
    if !failed.is_empty() {
        return (
            false,
            Some(format!(
                "verification receipt `{}` has failed command(s): {}",
                verification_receipt_path.display(),
                failed
                    .iter()
                    .map(|command| format!("`{command}`"))
                    .collect::<Vec<_>>()
                    .join(", ")
            )),
        );
    }

    let mut zero_test = expected_commands
        .iter()
        .filter(|command| {
            receipt
                .commands
                .iter()
                .filter(|entry| verification_receipt_command_matches(entry, command))
                .any(verification_receipt_reports_zero_tests)
        })
        .cloned()
        .collect::<Vec<_>>();
    zero_test.sort();
    if !zero_test.is_empty() {
        return (
            false,
            Some(format!(
                "verification receipt `{}` reported zero-test run(s): {}",
                verification_receipt_path.display(),
                zero_test
                    .iter()
                    .map(|command| format!("`{command}`"))
                    .collect::<Vec<_>>()
                    .join(", ")
            )),
        );
    }

    let mut unsuperseded_failed = receipt
        .commands
        .iter()
        .enumerate()
        .filter(|(_, entry)| !verification_receipt_command_passed(entry))
        .filter(|(entry_index, entry)| {
            !verification_receipt_failed_entry_is_superseded(
                *entry_index,
                entry,
                &receipt.commands,
                expected_commands,
            )
        })
        // Only DECLARED verification commands and compile/test-family commands
        // hard-gate completion. An incidental `rg`/`grep`/shell auxiliary a
        // worker ran (e.g. a Review/closeout absence check) exiting non-zero is
        // NOT a completion blocker: a search command exiting 1 means "no match",
        // which is frequently the desired closeout outcome, and real regressions
        // are already caught by the declared gate plus the separate workspace
        // test gate. This prevents an exit-inverted `rg X is empty` closeout
        // from self-blocking a task whose real verification passed.
        .filter(|(_, entry)| receipt_command_hard_gates(entry, expected_commands))
        .map(|(_, entry)| entry.command.clone())
        .collect::<Vec<_>>();
    unsuperseded_failed.sort();
    unsuperseded_failed.dedup();
    if !unsuperseded_failed.is_empty() {
        return (
            false,
            Some(format!(
                "verification receipt `{}` has unsuperseded failed command(s): {}",
                verification_receipt_path.display(),
                unsuperseded_failed
                    .iter()
                    .map(|command| format!("`{command}`"))
                    .collect::<Vec<_>>()
                    .join(", ")
            )),
        );
    }

    (true, None)
}

fn task_id_from_receipt_path(path: &Path) -> Option<String> {
    path.file_stem()
        .and_then(|stem| stem.to_str())
        .map(str::to_string)
}

fn verification_receipt_content_problem(
    verification_receipt_path: &Path,
    receipt: &VerificationReceipt,
    expected_commands: &[String],
) -> Option<String> {
    let mut missing = expected_commands
        .iter()
        .filter(|command| {
            !receipt
                .commands
                .iter()
                .any(|entry| verification_receipt_command_matches(entry, command))
        })
        .cloned()
        .collect::<Vec<_>>();
    missing.sort();
    if !missing.is_empty() {
        return Some(format!(
            "verification receipt `{}` is missing command(s): {}",
            verification_receipt_path.display(),
            missing
                .iter()
                .map(|command| format!("`{command}`"))
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }

    let mut failed = expected_commands
        .iter()
        .filter(|command| {
            let matching_entries = receipt
                .commands
                .iter()
                .filter(|entry| verification_receipt_command_matches(entry, command))
                .collect::<Vec<_>>();
            !matching_entries.is_empty()
                && matching_entries.iter().all(|entry| {
                    entry.status.as_deref() != Some("passed") || entry.exit_code != Some(0)
                })
        })
        .cloned()
        .collect::<Vec<_>>();
    failed.sort();
    if !failed.is_empty() {
        return Some(format!(
            "verification receipt `{}` has failed command(s): {}",
            verification_receipt_path.display(),
            failed
                .iter()
                .map(|command| format!("`{command}`"))
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }

    let mut zero_test = expected_commands
        .iter()
        .filter(|command| {
            receipt
                .commands
                .iter()
                .filter(|entry| verification_receipt_command_matches(entry, command))
                .any(verification_receipt_reports_zero_tests)
        })
        .cloned()
        .collect::<Vec<_>>();
    zero_test.sort();
    if !zero_test.is_empty() {
        return Some(format!(
            "verification receipt `{}` reported zero-test run(s): {}",
            verification_receipt_path.display(),
            zero_test
                .iter()
                .map(|command| format!("`{command}`"))
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }

    let mut unsuperseded_failed = receipt
        .commands
        .iter()
        .enumerate()
        .filter(|(_, entry)| !verification_receipt_command_passed(entry))
        .filter(|(entry_index, entry)| {
            !verification_receipt_failed_entry_is_superseded(
                *entry_index,
                entry,
                &receipt.commands,
                expected_commands,
            )
        })
        // Only DECLARED verification commands and compile/test-family commands
        // hard-gate completion. An incidental `rg`/`grep`/shell auxiliary a
        // worker ran (e.g. a Review/closeout absence check) exiting non-zero is
        // NOT a completion blocker: a search command exiting 1 means "no match",
        // which is frequently the desired closeout outcome, and real regressions
        // are already caught by the declared gate plus the separate workspace
        // test gate. This prevents an exit-inverted `rg X is empty` closeout
        // from self-blocking a task whose real verification passed.
        .filter(|(_, entry)| receipt_command_hard_gates(entry, expected_commands))
        .map(|(_, entry)| entry.command.clone())
        .collect::<Vec<_>>();
    unsuperseded_failed.sort();
    unsuperseded_failed.dedup();
    if !unsuperseded_failed.is_empty() {
        return Some(format!(
            "verification receipt `{}` has unsuperseded failed command(s): {}",
            verification_receipt_path.display(),
            unsuperseded_failed
                .iter()
                .map(|command| format!("`{command}`"))
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }

    None
}

pub(crate) fn shared_receipt_freshness_problem(
    repo_root: &Path,
    verification_receipt_path: &Path,
    receipt_text: &str,
    expected_commands: &[String],
    declared_artifacts: &[String],
) -> Result<Option<String>> {
    let receipt = serde_json::from_str::<VerificationReceipt>(receipt_text).with_context(|| {
        format!(
            "invalid verification receipt `{}`",
            verification_receipt_path.display()
        )
    })?;
    Ok(verification_receipt_freshness_problem(
        repo_root,
        verification_receipt_path,
        &receipt,
        expected_commands,
        declared_artifacts,
    ))
}

fn verification_receipt_command_passed(entry: &VerificationReceiptCommand) -> bool {
    entry.status.as_deref() == Some("passed") && entry.exit_code == Some(0)
}

fn verification_receipt_failed_entry_is_superseded(
    failed_index: usize,
    failed_entry: &VerificationReceiptCommand,
    all_entries: &[VerificationReceiptCommand],
    expected_commands: &[String],
) -> bool {
    all_entries.iter().enumerate().any(|(entry_index, entry)| {
        entry_index > failed_index
            && verification_receipt_command_passed(entry)
            && verification_receipt_commands_match_same_expected(
                failed_entry,
                entry,
                expected_commands,
            )
    }) || all_entries.iter().any(|entry| {
        verification_receipt_command_passed(entry)
            && expected_commands
                .iter()
                .any(|expected| verification_receipt_command_matches(entry, expected))
            && entry
                .supersedes
                .iter()
                .any(|superseded| superseded == &failed_entry.command)
    })
}

fn verification_receipt_commands_match_same_expected(
    left: &VerificationReceiptCommand,
    right: &VerificationReceiptCommand,
    expected_commands: &[String],
) -> bool {
    expected_commands.iter().any(|expected| {
        verification_receipt_command_matches(left, expected)
            && verification_receipt_command_matches(right, expected)
    })
}

fn verification_receipt_freshness_problem(
    repo_root: &Path,
    verification_receipt_path: &Path,
    receipt: &VerificationReceipt,
    expected_commands: &[String],
    declared_artifacts: &[String],
) -> Option<String> {
    verification_receipt_freshness_problem_for_source(
        repo_root,
        verification_receipt_path,
        receipt,
        expected_commands,
        declared_artifacts,
        VerificationReceiptSource::JsonFile,
    )
}

fn verification_receipt_freshness_problem_for_source(
    repo_root: &Path,
    verification_receipt_path: &Path,
    receipt: &VerificationReceipt,
    expected_commands: &[String],
    declared_artifacts: &[String],
    source: VerificationReceiptSource,
) -> Option<String> {
    let current_commit = current_git_commit(repo_root);
    let current_dirty_fingerprint = current_dirty_state_fingerprint(repo_root);
    let current_plan_hash = current_plan_hash(repo_root);
    let mut json_receipt_commit_is_current = false;

    if source == VerificationReceiptSource::JsonFile {
        if let Some(current) = current_commit {
            match receipt.commit.as_deref() {
                Some(recorded) if recorded == current => {
                    json_receipt_commit_is_current = true;
                }
                Some(recorded) if git_commit_is_ancestor(repo_root, recorded, &current) => {
                    json_receipt_commit_is_current = false;
                }
                Some(recorded) => {
                    return Some(format!(
                        "commit mismatch, recorded `{recorded}` is not current HEAD `{current}` or an ancestor"
                    ))
                }
                None => return Some("missing current commit metadata".to_string()),
            }
        }
    }

    if source == VerificationReceiptSource::JsonFile && json_receipt_commit_is_current {
        if let Some(current) = current_dirty_fingerprint {
            let Some(dirty_state) = receipt.dirty_state.as_ref() else {
                return Some("missing dirty-state fingerprint".to_string());
            };
            match dirty_state.fingerprint.as_deref() {
                Some(recorded) if recorded == current => {}
                Some(recorded) => {
                    return Some(format!(
                        "dirty-state fingerprint mismatch, recorded `{recorded}` but current fingerprint is `{current}`"
                    ))
                }
                None if dirty_state.entries.is_empty() && current_dirty_state_is_clean(repo_root) =>
                {
                }
                None => return Some("missing dirty-state fingerprint".to_string()),
            }
        }
    }

    if source == VerificationReceiptSource::JsonFile && json_receipt_commit_is_current {
        if let Some(current) = current_plan_hash {
            match receipt.plan_hash.as_deref() {
                Some(recorded) if recorded == current => {}
                Some(recorded) => {
                    return Some(format!(
                        "plan hash mismatch, recorded `{recorded}` but current IMPLEMENTATION_PLAN.md hash is `{current}`"
                    ))
                }
                None => return Some("missing plan hash".to_string()),
            }
        }
    }

    for (path, current_hash) in
        current_declared_artifact_hashes(repo_root, verification_receipt_path, declared_artifacts)
    {
        match receipt
            .declared_artifacts
            .iter()
            .find(|artifact| artifact.path == path)
            .and_then(|artifact| artifact.sha256.as_deref())
        {
            Some(recorded) if recorded == current_hash => {}
            Some(recorded) => {
                return Some(format!(
                    "declared artifact `{path}` hash mismatch, recorded `{recorded}` but current hash is `{current_hash}`"
                ))
            }
            None => return Some(format!("missing declared artifact `{path}` hash")),
        }
    }

    if source == VerificationReceiptSource::JsonFile && json_receipt_commit_is_current {
        for expected_command in expected_commands {
            if let Some(problem) = verification_command_argv_problem(receipt, expected_command) {
                return Some(problem);
            }
        }
    }

    None
}

fn verification_command_argv_problem(
    receipt: &VerificationReceipt,
    expected_command: &str,
) -> Option<String> {
    let expected_argv = shell_split(expected_command)?;
    let matching = receipt
        .commands
        .iter()
        .filter(|entry| verification_receipt_command_matches(entry, expected_command))
        .collect::<Vec<_>>();
    if matching.is_empty() {
        return None;
    }
    if matching.iter().any(|entry| {
        entry.expected_argv.as_ref().is_some_and(|argv| {
            argv == &expected_argv
                || unwrap_launcher_argv(argv).is_some_and(|unwrapped| {
                    unwrapped == expected_argv
                        || unwrap_launcher_argv(&unwrapped)
                            .is_some_and(|inner| inner == expected_argv)
                })
        })
    }) {
        return None;
    }
    Some(format!(
        "command `{expected_command}` is missing matching expected argv metadata"
    ))
}

fn current_git_commit(repo_root: &Path) -> Option<String> {
    command_stdout(repo_root, ["rev-parse", "HEAD"]).map(|value| value.trim().to_string())
}

fn current_dirty_state_fingerprint(repo_root: &Path) -> Option<String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(repo_root)
        .args(["status", "--porcelain=v1", "-z"])
        .output()
        .ok()?;
    output.status.success().then(|| sha256_hex(&output.stdout))
}

fn current_dirty_state_is_clean(repo_root: &Path) -> bool {
    Command::new("git")
        .arg("-C")
        .arg(repo_root)
        .args(["status", "--porcelain=v1", "-z"])
        .output()
        .is_ok_and(|output| output.status.success() && output.stdout.is_empty())
}

/// Normalize task-status checkbox markers (`[x]`/`[X]`/`[~]`/`[!]`) to the empty
/// form `[ ]` before hashing the plan. The host flips a task's checkbox on EVERY
/// landing, so a whole-file hash of `IMPLEMENTATION_PLAN.md` goes stale the
/// instant any task's status changes — through no fault of the worker — which is
/// the root of the spurious `[~]` "plan hash mismatch" bug class. By normalizing
/// the status glyph out, the hash tracks only genuine SPEC content (titles,
/// bodies, dependencies, verification commands, declared artifacts); a checkbox
/// flip never invalidates a receipt, but a real spec edit still does.
///
/// The tokens are ASCII, so this string replacement is byte-for-byte identical
/// to the same normalization in `scripts/verification_receipt.py` for any valid
/// UTF-8 plan file — the two MUST stay in lockstep or worker and host hashes
/// will never match.
pub(crate) fn normalize_plan_status_markers(text: &str) -> String {
    text.replace("[x]", "[ ]")
        .replace("[X]", "[ ]")
        .replace("[~]", "[ ]")
        .replace("[!]", "[ ]")
}

/// Hash plan bytes with status markers normalized (see
/// [`normalize_plan_status_markers`]). Shared by the freshness gate and the
/// host's lane-receipt propagation so every site agrees on one plan-hash value.
pub(crate) fn normalized_plan_hash_bytes(plan_bytes: &[u8]) -> String {
    let text = String::from_utf8_lossy(plan_bytes);
    sha256_hex(normalize_plan_status_markers(&text).as_bytes())
}

fn current_plan_hash(repo_root: &Path) -> Option<String> {
    fs::read(repo_root.join("IMPLEMENTATION_PLAN.md"))
        .ok()
        .map(|bytes| normalized_plan_hash_bytes(&bytes))
}

fn git_commit_is_ancestor(repo_root: &Path, ancestor: &str, descendant: &str) -> bool {
    Command::new("git")
        .arg("-C")
        .arg(repo_root)
        .args(["merge-base", "--is-ancestor", ancestor, descendant])
        .output()
        .map(|output| output.status.success())
        .unwrap_or(false)
}

fn command_stdout<const N: usize>(repo_root: &Path, args: [&str; N]) -> Option<String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(repo_root)
        .args(args)
        .output()
        .ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).to_string())
}

fn verification_receipt_reports_zero_tests(entry: &VerificationReceiptCommand) -> bool {
    entry.status.as_deref() == Some("passed")
        && entry.exit_code == Some(0)
        && entry
            .runner_summary
            .as_ref()
            .is_some_and(|summary| summary.zero_test_detected)
}

/// A `VAR=value` shell env-assignment token (leading env prefix).
fn is_env_assignment(token: &str) -> bool {
    match token.split_once('=') {
        Some((name, _)) => {
            !name.is_empty()
                && name
                    .chars()
                    .next()
                    .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
                && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
        }
        None => false,
    }
}

/// Whether a FAILED receipt command should block task completion. Declared
/// verification commands always gate. Otherwise, only compile/test-family
/// commands (cargo test/build/check/clippy/bench/nextest, or a standard test
/// runner) gate — an incidental search/shell auxiliary a worker ran does not,
/// because its exit code is not a reliable pass/fail signal for completion.
fn receipt_command_hard_gates(
    entry: &VerificationReceiptCommand,
    expected_commands: &[String],
) -> bool {
    if expected_commands
        .iter()
        .any(|expected| verification_receipt_command_matches(entry, expected))
    {
        return true;
    }
    // Resolve the real argv, unwrapping env/shell launchers.
    let mut argv = if !entry.argv.is_empty() {
        entry.argv.clone()
    } else {
        shell_split(&entry.command).unwrap_or_default()
    };
    for _ in 0..2 {
        match unwrap_launcher_argv(&argv) {
            Some(inner) => argv = inner,
            None => break,
        }
    }
    // Drop a leading VAR=value env prefix.
    let program = argv
        .iter()
        .find(|tok| !is_env_assignment(tok))
        .map(|tok| {
            Path::new(tok)
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or(tok.as_str())
        });
    let Some(program) = program else {
        return false;
    };
    match program {
        "cargo" => {
            // cargo <subcommand> — gate on the compile/test-family subcommands.
            let sub = argv
                .iter()
                .skip_while(|tok| is_env_assignment(tok))
                .nth(1)
                .map(String::as_str);
            matches!(
                sub,
                Some("test" | "build" | "check" | "clippy" | "bench" | "nextest")
            )
        }
        "pytest" | "go" | "jest" | "vitest" | "mocha" | "gradle" | "mvn" | "ctest" | "make" => true,
        _ => false,
    }
}

fn verification_receipt_command_matches(
    entry: &VerificationReceiptCommand,
    expected_command: &str,
) -> bool {
    if entry.command == expected_command {
        return true;
    }

    let expected_argv = match shell_split(expected_command) {
        Some(argv) => argv,
        None => return false,
    };

    let entry_argv = if !entry.argv.is_empty() {
        Some(entry.argv.clone())
    } else {
        shell_split(&entry.command)
    };
    let Some(mut candidate) = entry_argv else {
        return false;
    };
    if expected_argv == candidate {
        return true;
    }
    // Workers legitimately wrap env-var-prefixed verification commands in a
    // shell launcher (`bash -lc "<cmd>"`) or an `env` launcher because a
    // leading VAR=value token cannot exec as bare argv. Unwrap launcher
    // shapes (at most twice: `bash -lc "env VAR=... cmd"`) before comparing
    // so the recorded run still matches the plan row's literal command.
    for _ in 0..2 {
        let Some(next) = unwrap_launcher_argv(&candidate) else {
            return false;
        };
        if expected_argv == next {
            return true;
        }
        candidate = next;
    }
    false
}

fn unwrap_launcher_argv(argv: &[String]) -> Option<Vec<String>> {
    let arg0 = argv.first().map(|arg0| {
        Path::new(arg0)
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or(arg0.as_str())
    })?;
    match arg0 {
        "bash" | "sh" | "zsh" | "dash"
            if argv.len() == 3 && matches!(argv[1].as_str(), "-c" | "-lc" | "-cl") =>
        {
            shell_split(&argv[2])
        }
        "env" if argv.len() > 1 => Some(argv[1..].to_vec()),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::PathBuf;
    use std::process::Command;

    use crate::completion_artifacts::artifacts::artifact_hash;

    use super::{
        normalized_plan_hash_bytes, verification_receipt_freshness_problem, VerificationDirtyState,
        VerificationReceipt, VerificationReceiptArtifact, VerificationReceiptCommand,
    };

    #[test]
    fn normalized_plan_hash_is_stable_across_checkbox_flips_but_not_spec_edits() {
        let pending = b"# Plan\n- [ ] `TASK-1` Do the thing\n- [ ] `TASK-2` Other\n";
        let partial = b"# Plan\n- [~] `TASK-1` Do the thing\n- [ ] `TASK-2` Other\n";
        let done = b"# Plan\n- [x] `TASK-1` Do the thing\n- [x] `TASK-2` Other\n";
        // Flipping any/all checkboxes must NOT change the hash — that was the
        // root of the spurious "[~] plan hash mismatch" bug.
        assert_eq!(
            normalized_plan_hash_bytes(pending),
            normalized_plan_hash_bytes(partial)
        );
        assert_eq!(
            normalized_plan_hash_bytes(pending),
            normalized_plan_hash_bytes(done)
        );
        // A genuine spec edit (changing a task title) MUST change the hash.
        let edited = b"# Plan\n- [x] `TASK-1` Do the OTHER thing\n- [x] `TASK-2` Other\n";
        assert_ne!(
            normalized_plan_hash_bytes(done),
            normalized_plan_hash_bytes(edited)
        );
    }

    fn temp_dir(name: &str) -> PathBuf {
        let mut path = std::env::temp_dir();
        path.push(format!(
            "autodev-completion-artifacts-{name}-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).expect("failed to create temp dir");
        path
    }

    fn init_git_repo(root: &std::path::Path) {
        Command::new("git")
            .arg("-C")
            .arg(root)
            .arg("init")
            .output()
            .expect("git init failed");
        Command::new("git")
            .arg("-C")
            .arg(root)
            .args(["config", "user.email", "test@example.com"])
            .output()
            .expect("git config email failed");
        Command::new("git")
            .arg("-C")
            .arg(root)
            .args(["config", "user.name", "Test User"])
            .output()
            .expect("git config name failed");
        fs::write(root.join("IMPLEMENTATION_PLAN.md"), "# plan\n").expect("failed to write plan");
        Command::new("git")
            .arg("-C")
            .arg(root)
            .args(["add", "IMPLEMENTATION_PLAN.md"])
            .output()
            .expect("git add failed");
        Command::new("git")
            .arg("-C")
            .arg(root)
            .args(["commit", "-m", "initial"])
            .output()
            .expect("git commit failed");
    }

    #[test]
    fn verification_receipt_freshness_requires_current_tree_metadata() {
        let root = temp_dir("current-tree-metadata-receipt");
        init_git_repo(&root);
        fs::create_dir_all(root.join(".auto/symphony/verification-receipts"))
            .expect("failed to create receipts dir");
        fs::create_dir_all(root.join("docs/ops")).expect("failed to create docs dir");
        fs::write(root.join("docs/ops/proof.md"), "receipt proof\n")
            .expect("failed to write proof");
        let receipt_path = root.join(".auto/symphony/verification-receipts/SAT-003.json");
        fs::write(&receipt_path, "{}\n").expect("failed to write receipt placeholder");

        let commit = super::current_git_commit(&root).expect("git commit should be readable");
        let dirty_fingerprint = super::current_dirty_state_fingerprint(&root)
            .expect("dirty-state fingerprint should be readable");
        let plan_hash = super::current_plan_hash(&root).expect("plan hash should be readable");
        let artifact_hash = artifact_hash(&root.join("docs/ops/proof.md"))
            .expect("artifact hash should be readable");
        let expected_command =
            "cargo test completion_artifacts::tests::metadata_receipt".to_string();
        let expected_argv = vec![
            "cargo".to_string(),
            "test".to_string(),
            "completion_artifacts::tests::metadata_receipt".to_string(),
        ];
        let base_receipt = VerificationReceipt {
            task_id: Some("TASK-METADATA".to_string()),
            commit: Some(commit.clone()),
            dirty_state: Some(VerificationDirtyState {
                fingerprint: Some(dirty_fingerprint.clone()),
                ..VerificationDirtyState::default()
            }),
            plan_hash: Some(plan_hash.clone()),
            declared_artifacts: vec![VerificationReceiptArtifact {
                path: "docs/ops/proof.md".to_string(),
                sha256: Some(artifact_hash.clone()),
            }],
            commands: vec![VerificationReceiptCommand {
                command: expected_command.clone(),
                expected_argv: Some(expected_argv),
                exit_code: Some(0),
                status: Some("passed".to_string()),
                ..VerificationReceiptCommand::default()
            }],
        };
        let expected_commands = std::slice::from_ref(&expected_command);
        let declared_artifacts = vec!["docs/ops/proof.md".to_string()];

        assert_eq!(
            verification_receipt_freshness_problem(
                &root,
                &receipt_path,
                &base_receipt,
                expected_commands,
                &declared_artifacts,
            ),
            None
        );

        let cases = [
            (
                {
                    let mut receipt = base_receipt.clone();
                    receipt.commit = None;
                    receipt
                },
                "missing current commit metadata",
            ),
            (
                {
                    let mut receipt = base_receipt.clone();
                    receipt.dirty_state = None;
                    receipt
                },
                "missing dirty-state fingerprint",
            ),
            (
                {
                    let mut receipt = base_receipt.clone();
                    receipt.dirty_state = Some(VerificationDirtyState {
                        fingerprint: None,
                        entries: vec![serde_json::Value::String(" M src/main.rs".to_string())],
                    });
                    receipt
                },
                "missing dirty-state fingerprint",
            ),
            (
                {
                    let mut receipt = base_receipt.clone();
                    receipt.plan_hash = None;
                    receipt
                },
                "missing plan hash",
            ),
            (
                {
                    let mut receipt = base_receipt.clone();
                    receipt.declared_artifacts[0].sha256 = None;
                    receipt
                },
                "missing declared artifact `docs/ops/proof.md` hash",
            ),
            (
                {
                    let mut receipt = base_receipt.clone();
                    receipt.commands[0].expected_argv = None;
                    receipt
                },
                "missing matching expected argv metadata",
            ),
        ];

        for (receipt, expected_problem) in cases {
            let problem = verification_receipt_freshness_problem(
                &root,
                &receipt_path,
                &receipt,
                expected_commands,
                &declared_artifacts,
            )
            .expect("receipt should be stale");
            assert!(
                problem.contains(expected_problem),
                "expected `{problem}` to contain `{expected_problem}`"
            );
        }
    }

    #[test]
    fn verification_receipt_freshness_accepts_legacy_clean_dirty_entries() {
        let root = temp_dir("legacy-clean-dirty-entries");
        init_git_repo(&root);
        let receipt_path = root.join(".auto/symphony/verification-receipts/SAT-LEGACY.json");
        let expected_command = "npm run typecheck".to_string();
        let receipt = VerificationReceipt {
            task_id: Some("TASK-LEGACY".to_string()),
            commit: super::current_git_commit(&root),
            dirty_state: Some(VerificationDirtyState {
                fingerprint: None,
                entries: Vec::new(),
            }),
            plan_hash: super::current_plan_hash(&root),
            declared_artifacts: Vec::new(),
            commands: vec![VerificationReceiptCommand {
                command: expected_command.clone(),
                expected_argv: Some(vec![
                    "npm".to_string(),
                    "run".to_string(),
                    "typecheck".to_string(),
                ]),
                exit_code: Some(0),
                status: Some("passed".to_string()),
                ..VerificationReceiptCommand::default()
            }],
        };

        assert_eq!(
            verification_receipt_freshness_problem(
                &root,
                &receipt_path,
                &receipt,
                std::slice::from_ref(&expected_command),
                &[],
            ),
            None
        );

        fs::write(root.join("dirty.txt"), "dirty\n").expect("failed to dirty repo");
        let problem = verification_receipt_freshness_problem(
            &root,
            &receipt_path,
            &receipt,
            std::slice::from_ref(&expected_command),
            &[],
        )
        .expect("dirty repo should reject legacy clean state");
        assert!(problem.contains("missing dirty-state fingerprint"));
    }

    #[test]
    fn verification_receipt_freshness_ignores_mutable_handoff_artifact_hashes() {
        let root = temp_dir("mutable-handoff-artifact-hash");
        init_git_repo(&root);
        fs::create_dir_all(root.join(".auto/symphony/verification-receipts"))
            .expect("failed to create receipts dir");
        fs::write(root.join("REVIEW.md"), "# REVIEW\n\nold\n").expect("failed to write review");
        let receipt_path = root.join(".auto/symphony/verification-receipts/SAT-004.json");
        fs::write(&receipt_path, "{}\n").expect("failed to write receipt placeholder");
        let expected_command = "cargo test completion_artifacts::tests::mutable_review".to_string();
        let expected_argv = vec![
            "cargo".to_string(),
            "test".to_string(),
            "completion_artifacts::tests::mutable_review".to_string(),
        ];
        let mut receipt = VerificationReceipt {
            task_id: Some("TASK-MUTABLE".to_string()),
            commit: super::current_git_commit(&root),
            dirty_state: Some(VerificationDirtyState {
                fingerprint: super::current_dirty_state_fingerprint(&root),
                ..VerificationDirtyState::default()
            }),
            plan_hash: super::current_plan_hash(&root),
            declared_artifacts: vec![VerificationReceiptArtifact {
                path: "REVIEW.md".to_string(),
                sha256: Some("not-the-current-review-hash".to_string()),
            }],
            commands: vec![VerificationReceiptCommand {
                command: expected_command.clone(),
                expected_argv: Some(expected_argv),
                exit_code: Some(0),
                status: Some("passed".to_string()),
                ..VerificationReceiptCommand::default()
            }],
        };
        receipt.dirty_state = Some(VerificationDirtyState {
            fingerprint: super::current_dirty_state_fingerprint(&root),
            ..VerificationDirtyState::default()
        });

        assert_eq!(
            verification_receipt_freshness_problem(
                &root,
                &receipt_path,
                &receipt,
                std::slice::from_ref(&expected_command),
                &["REVIEW.md".to_string()],
            ),
            None
        );
    }

    #[test]
    fn receipt_schema_requires_current_metadata() {
        let schema = fs::read_to_string(
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("docs/verification-receipt-schema.md"),
        )
        .expect("schema should exist");
        for field in [
            "commit",
            "dirty_state.fingerprint",
            "plan_hash",
            "expected_argv",
            "declared_artifacts",
        ] {
            assert!(schema.contains(field), "schema should mention `{field}`");
        }
    }

    #[test]
    fn receipt_command_accepts_exit_status_alias() {
        let command = serde_json::from_str::<VerificationReceiptCommand>(
            r#"{"command":"npm run typecheck","exit_status":0,"status":"passed"}"#,
        )
        .expect("legacy exit_status command should parse");

        assert_eq!(command.exit_code, Some(0));
        assert!(super::verification_receipt_command_passed(&command));
    }

    #[test]
    fn receipt_artifacts_accept_completion_artifacts_alias() {
        let receipt = serde_json::from_str::<VerificationReceipt>(
            r#"{"completion_artifacts":[{"path":"proof.md","sha256":"abc123"}]}"#,
        )
        .expect("legacy completion_artifacts receipt should parse");

        assert_eq!(receipt.declared_artifacts.len(), 1);
        assert_eq!(receipt.declared_artifacts[0].path, "proof.md");
        assert_eq!(
            receipt.declared_artifacts[0].sha256.as_deref(),
            Some("abc123")
        );
    }

    #[test]
    fn shared_receipt_inspector_rejects_non_ancestor_commit() {
        let root = temp_dir("shared-non-ancestor-receipt");
        init_git_repo(&root);
        let receipt_path = root.join(".auto/symphony/verification-receipts/TASK.json");
        let receipt_text = format!(
            r#"{{"commit":"{}","commands":[{{"command":"cargo test completion_artifacts::tests::shared_receipt","expected_argv":["cargo","test","completion_artifacts::tests::shared_receipt"],"exit_code":0,"status":"passed"}}]}}"#,
            "1111111111111111111111111111111111111111"
        );
        let problem = super::shared_receipt_freshness_problem(
            &root,
            &receipt_path,
            &receipt_text,
            &["cargo test completion_artifacts::tests::shared_receipt".to_string()],
            &[],
        )
        .expect("receipt should parse")
        .expect("non-ancestor commit rejected");
        assert!(problem.contains("commit mismatch"));
    }

    fn cmd(command: &str, argv: &[&str]) -> VerificationReceiptCommand {
        VerificationReceiptCommand {
            command: command.to_string(),
            argv: argv.iter().map(|s| s.to_string()).collect(),
            exit_code: Some(1),
            status: Some("failed".to_string()),
            ..VerificationReceiptCommand::default()
        }
    }

    #[test]
    fn declared_failed_command_hard_gates() {
        let entry = cmd("cargo test -p x t", &["cargo", "test", "-p", "x", "t"]);
        let expected = vec!["cargo test -p x t".to_string()];
        assert!(super::receipt_command_hard_gates(&entry, &expected));
    }

    #[test]
    fn failed_cargo_test_hard_gates_even_if_not_declared() {
        let entry = cmd("cargo test -p other", &["cargo", "test", "-p", "other"]);
        assert!(super::receipt_command_hard_gates(&entry, &[]));
    }

    #[test]
    fn incidental_ripgrep_absence_check_does_not_gate() {
        // The exact self-block class: a bare `rg` closeout absence check that
        // exited 1 (no match = desired) must NOT block completion.
        let entry = cmd(
            "rg certificate_for_block crates/x/src",
            &["rg", "certificate_for_block", "crates/x/src"],
        );
        assert!(!super::receipt_command_hard_gates(&entry, &[]));
    }

    #[test]
    fn incidental_negated_shell_grep_does_not_gate() {
        let entry = cmd(
            "bash -lc <neg>",
            &["bash", "-lc", "! rg -q LocalValidatorFleet crates/x"],
        );
        assert!(!super::receipt_command_hard_gates(&entry, &[]));
    }

    #[test]
    fn env_prefixed_cargo_build_still_gates() {
        let entry = cmd(
            "RUSTFLAGS=--cfg x cargo build",
            &["RUSTFLAGS=--cfg x", "cargo", "build"],
        );
        assert!(super::receipt_command_hard_gates(&entry, &[]));
    }

    #[test]
    fn command_matches_unwraps_bash_lc_launcher_for_env_prefixed_commands() {
        let expected =
            r#"RUSTFLAGS="--cfg commonware_stability_BETA" cargo check -p rsociety-chain"#;
        let entry = VerificationReceiptCommand {
            command: "bash -lc <quoted>".to_string(),
            argv: vec![
                "bash".to_string(),
                "-lc".to_string(),
                expected.to_string(),
            ],
            ..VerificationReceiptCommand::default()
        };
        assert!(super::verification_receipt_command_matches(
            &entry, expected
        ));
    }

    #[test]
    fn command_matches_unwraps_env_launcher() {
        let expected = r#"RUSTFLAGS="--cfg beta" cargo check -p demo"#;
        let entry = VerificationReceiptCommand {
            command: "env launcher".to_string(),
            argv: vec![
                "env".to_string(),
                "RUSTFLAGS=--cfg beta".to_string(),
                "cargo".to_string(),
                "check".to_string(),
                "-p".to_string(),
                "demo".to_string(),
            ],
            ..VerificationReceiptCommand::default()
        };
        assert!(super::verification_receipt_command_matches(
            &entry, expected
        ));
    }

    #[test]
    fn command_matches_rejects_launcher_with_different_inner_command() {
        let expected = r#"RUSTFLAGS="--cfg beta" cargo check -p demo"#;
        let entry = VerificationReceiptCommand {
            command: "bash -lc other".to_string(),
            argv: vec![
                "bash".to_string(),
                "-lc".to_string(),
                "cargo test -p other-crate".to_string(),
            ],
            ..VerificationReceiptCommand::default()
        };
        assert!(!super::verification_receipt_command_matches(
            &entry, expected
        ));
    }
}
