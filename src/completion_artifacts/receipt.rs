//! Verification-receipt model, footer codec, freshness checks, and inspection.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use anyhow::{bail, Context, Result};
use base64::Engine;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use shlex::split as shell_split;

use crate::completion_artifacts::artifacts::{
    current_declared_artifact_hashes, declared_artifact_path, sha256_hex,
};
use crate::completion_artifacts::review_contains_task;
use crate::completion_artifacts::verification::verification_plan;
use crate::task_parser::{parse_tasks, TaskStatus};
use crate::util::atomic_write;

const RECEIPT_FOOTER_VERSION: &str = "Auto-Verification-Receipt-Version:";
const RECEIPT_FOOTER_TASK: &str = "Auto-Verification-Receipt-Task:";
const RECEIPT_FOOTER_JSON: &str = "Auto-Verification-Receipt-JSON:";
const VERIFIED_SOURCE_ATTESTATION_VERSION: u32 = 2;
const SOURCE_STATE_MAX_SUBMODULE_DEPTH: usize = 8;
const SOURCE_STATE_MAX_ENTRIES: usize = 200_000;
const SOURCE_STATE_MAX_BYTES: u64 = 1024 * 1024 * 1024;
const LEGACY_CLEAN_PORCELAIN_SHA256: &str =
    "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";
const HOST_QUEUE_STATE_FILES: [&str; 7] = [
    "IMPLEMENTATION_PLAN.md",
    "COMPLETED.md",
    "WORKLIST.md",
    "REVIEW.md",
    "AGENTS.md",
    "ARCHIVED.md",
    "RECEIPTS-DRIFT.md",
];

pub(crate) fn verification_receipt_path(repo_root: &Path, task_id: &str) -> PathBuf {
    verification_receipt_root(repo_root).join(format!("{task_id}.json"))
}

fn verified_source_attestation_path(repo_root: &Path, task_id: &str) -> PathBuf {
    repo_root
        .join(".auto/parallel/verified-source")
        .join(format!("{task_id}.json"))
}

pub(crate) fn clear_verified_source_attestation(repo_root: &Path, task_id: &str) -> Result<()> {
    let path = verified_source_attestation_path(repo_root, task_id);
    match fs::remove_file(&path) {
        Ok(()) => Ok(()),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(err) => Err(err).with_context(|| format!("failed to remove {}", path.display())),
    }
}

pub(crate) fn record_verified_source_attestation(repo_root: &Path, task_id: &str) -> Result<()> {
    record_verified_source_attestation_with_source_limits(
        repo_root,
        task_id,
        SourceStateLimits::default(),
    )
}

fn record_verified_source_attestation_with_source_limits(
    repo_root: &Path,
    task_id: &str,
    source_limits: SourceStateLimits,
) -> Result<()> {
    let mut budget = SourceStateBudget::default();
    let plan_path = repo_root.join("IMPLEMENTATION_PLAN.md");
    let plan_text = read_bounded_utf8_file(
        &plan_path,
        source_limits,
        &mut budget,
        "attestation IMPLEMENTATION_PLAN.md input",
    )
    .context("cannot attest verified source without IMPLEMENTATION_PLAN.md")?;
    let matching_tasks = parse_tasks(&plan_text)
        .into_iter()
        .filter(|task| task.id == task_id)
        .collect::<Vec<_>>();
    let [task] = matching_tasks.as_slice() else {
        bail!(
            "cannot attest verified source for `{task_id}`: expected exactly one matching plan row, found {}",
            matching_tasks.len()
        );
    };
    let verification = verification_plan(&task.markdown);
    if verification.executable_commands.is_empty() {
        bail!("cannot attest verified source for `{task_id}` without executable verification");
    }

    let receipt_path = verification_receipt_path(repo_root, task_id);
    let receipt_text = read_bounded_utf8_file(
        &receipt_path,
        source_limits,
        &mut budget,
        "attestation verification receipt input",
    )
    .with_context(|| format!("failed to read {}", receipt_path.display()))?;
    let mut bounded_freshness = BoundedFreshnessContext {
        limits: source_limits,
        budget: &mut budget,
        plan_input: plan_text.as_bytes(),
        source_state: None,
    };
    if let Some(problem) = direct_verification_receipt_problem_with_bounded_freshness(
        repo_root,
        &receipt_path,
        &receipt_text,
        &verification.executable_commands,
        &task.completion_artifacts,
        &mut bounded_freshness,
    )? {
        bail!("cannot attest verified source for `{task_id}`: {problem}");
    }

    let source_state_v2 = match bounded_freshness.source_state.take() {
        Some(fingerprint) => fingerprint,
        None => current_source_state_fingerprint_with_budget_and_plan(
            repo_root,
            plan_text.as_bytes(),
            source_limits,
            bounded_freshness.budget,
        )
        .with_context(|| {
            format!("cannot compute source-state fingerprint while attesting `{task_id}`")
        })?,
    };
    let receipt = serde_json::from_str::<VerificationReceipt>(&receipt_text)
        .with_context(|| format!("invalid verification receipt `{}`", receipt_path.display()))?;
    let attestation = VerifiedSourceAttestation {
        version: VERIFIED_SOURCE_ATTESTATION_VERSION,
        task_id: task_id.to_string(),
        source_state_v2,
        receipt_proof_sha256: verification_proof_payload_sha256(&receipt)?,
        expected_commands: verification.executable_commands,
    };
    let rendered = serde_json::to_vec_pretty(&attestation)
        .context("failed to serialize verified-source attestation")?;
    let path = verified_source_attestation_path(repo_root, task_id);
    atomic_write(&path, &rendered).with_context(|| format!("failed to write {}", path.display()))
}

pub(crate) fn verification_receipt_commit_footer(
    repo_root: &Path,
    task_id: &str,
) -> Result<Option<String>> {
    verification_receipt_commit_footer_with_source_limits(
        repo_root,
        task_id,
        SourceStateLimits::default(),
    )
}

fn verification_receipt_commit_footer_with_source_limits(
    repo_root: &Path,
    task_id: &str,
    source_limits: SourceStateLimits,
) -> Result<Option<String>> {
    let path = verification_receipt_path(repo_root, task_id);
    if !path.exists() {
        return Ok(None);
    }
    let mut budget = SourceStateBudget::default();
    let receipt_text = read_bounded_utf8_file(
        &path,
        source_limits,
        &mut budget,
        "footer verification receipt input",
    )
    .with_context(|| format!("failed to read {}", path.display()))?;
    let receipt = serde_json::from_str::<VerificationReceipt>(&receipt_text)
        .with_context(|| format!("invalid verification receipt `{}`", path.display()))?;
    match receipt.task_id.as_deref() {
        Some(recorded) if recorded == task_id => {}
        Some(recorded) => bail!(
            "verification receipt `{}` task_id `{recorded}` does not match requested footer task `{task_id}`",
            path.display()
        ),
        None => bail!(
            "verification receipt `{}` is missing task_id for requested footer task `{task_id}`",
            path.display()
        ),
    }

    let plan_path = repo_root.join("IMPLEMENTATION_PLAN.md");
    let plan_text = read_bounded_utf8_file(
        &plan_path,
        source_limits,
        &mut budget,
        "footer IMPLEMENTATION_PLAN.md input",
    )
    .context("cannot prepare a durable verification footer without IMPLEMENTATION_PLAN.md")?;
    let matching_tasks = parse_tasks(&plan_text)
        .into_iter()
        .filter(|task| task.id == task_id)
        .collect::<Vec<_>>();
    let [task] = matching_tasks.as_slice() else {
        bail!(
            "cannot prepare durable verification footer for `{task_id}`: expected exactly one matching plan row, found {}",
            matching_tasks.len()
        );
    };
    if task.status != TaskStatus::Done {
        return Ok(None);
    }
    let verification = verification_plan(&task.markdown);
    if verification.executable_commands.is_empty() {
        bail!(
            "cannot prepare durable verification footer for `{task_id}` without executable verification commands"
        );
    }
    if let Some(problem) =
        verification_receipt_content_problem(&path, &receipt, &verification.executable_commands)
    {
        bail!("{problem}");
    }
    let mut source_context = BoundedFreshnessContext {
        limits: source_limits,
        budget: &mut budget,
        plan_input: plan_text.as_bytes(),
        source_state: None,
    };
    let verified_source_state = require_current_source_attestation_for_footer(
        repo_root,
        task_id,
        &path,
        &receipt,
        &verification.executable_commands,
        &mut source_context,
    )?;
    for artifact in &task.completion_artifacts {
        if declared_artifact_path(repo_root, artifact).is_none() {
            bail!(
                "cannot prepare durable verification footer for `{task_id}`: missing declared completion artifact `{artifact}`"
            );
        }
    }
    for (artifact, current_hash) in
        current_declared_artifact_hashes(repo_root, &path, &task.completion_artifacts)
    {
        let matches = receipt
            .declared_artifacts
            .iter()
            .find(|record| record.path == artifact)
            .and_then(|record| record.sha256.as_deref())
            .is_some_and(|recorded| recorded == current_hash);
        if !matches {
            bail!(
                "cannot prepare durable verification footer for `{task_id}`: missing or stale declared artifact hash for `{artifact}`"
            );
        }
    }
    // Stamp the versioned per-task owned-inputs fingerprint (computed against
    // the plan being committed) so future drift sweeps can trust this receipt
    // without re-running its verification when the task's own inputs are
    // unchanged. Absent when the task is not in the plan or git enumeration
    // fails — a receipt without the field falls back to legacy behavior.
    let owned_inputs_fp = task_owned_inputs_fingerprint_for(repo_root, task_id);
    let compact = compact_receipt_json_for_footer(
        &receipt_text,
        owned_inputs_fp.as_deref(),
        Some(&verified_source_state),
    )
    .with_context(|| format!("failed to prepare receipt footer from {}", path.display()))?;
    let encoded = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(compact.as_bytes());
    Ok(Some(format!(
        "{RECEIPT_FOOTER_VERSION} 1\n{RECEIPT_FOOTER_TASK} {task_id}\n{RECEIPT_FOOTER_JSON} {encoded}"
    )))
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
            let footer = parse_verification_receipt_footer(commit.trim(), body)?;
            verification_receipt_footer_has_host_provenance(repo_root, body, &footer)
                .then_some(footer)
        })
        .collect()
}

pub(crate) fn commit_message_has_reserved_verification_receipt_footer(body: &str) -> bool {
    body.lines().any(|line| {
        let line = line.trim();
        line.starts_with(RECEIPT_FOOTER_VERSION)
            || line.starts_with(RECEIPT_FOOTER_TASK)
            || line.starts_with(RECEIPT_FOOTER_JSON)
    })
}

fn verification_receipt_footer_has_host_provenance(
    repo_root: &Path,
    body: &str,
    footer: &VerificationReceiptFooter,
) -> bool {
    let subject = body.lines().next().map(str::trim).unwrap_or_default();
    let queue_sync = subject.ends_with(&format!(": {} queue sync", footer.task_id));
    let backfill = subject.ends_with(&format!(": {} receipt footer backfill", footer.task_id));
    if (!queue_sync && !backfill) || subject.starts_with(':') {
        return false;
    }

    let Some(parent_line) = git_command_stdout(
        repo_root,
        ["rev-list", "--parents", "-n", "1", &footer.commit],
    ) else {
        return false;
    };
    if parent_line.split_whitespace().count() != 2 {
        return false;
    }
    let Some(parent_commit) = parent_line.split_whitespace().nth(1) else {
        return false;
    };

    let Some(changed) = git_command_stdout(
        repo_root,
        [
            "diff-tree",
            "--root",
            "--no-commit-id",
            "--name-only",
            "-r",
            &footer.commit,
        ],
    ) else {
        return false;
    };
    let changed_paths = changed
        .lines()
        .map(str::trim)
        .filter(|path| !path.is_empty())
        .collect::<Vec<_>>();
    if changed_paths
        .iter()
        .any(|path| !HOST_QUEUE_STATE_FILES.contains(path))
        || (queue_sync && changed_paths.is_empty())
        || (backfill && !changed_paths.is_empty())
    {
        return false;
    }

    let Some(plan_text) = git_show_file(repo_root, &footer.commit, "IMPLEMENTATION_PLAN.md") else {
        return false;
    };
    let Some(parent_plan_text) = git_show_file(repo_root, parent_commit, "IMPLEMENTATION_PLAN.md")
    else {
        return false;
    };
    if !footer_task_transition_is_scoped(&parent_plan_text, &plan_text, &footer.task_id) {
        return false;
    }
    let matching_tasks = parse_tasks(&plan_text)
        .into_iter()
        .filter(|task| task.id == footer.task_id)
        .collect::<Vec<_>>();
    let [task] = matching_tasks.as_slice() else {
        return false;
    };
    if task.status != TaskStatus::Done {
        return false;
    }

    let Some(review_text) = git_show_file(repo_root, &footer.commit, "REVIEW.md") else {
        return false;
    };
    if !review_contains_task(&review_text, &footer.task_id) {
        return false;
    }

    let Ok(receipt) = serde_json::from_str::<VerificationReceipt>(&footer.receipt_text) else {
        return false;
    };
    if receipt.task_id.as_deref() != Some(footer.task_id.as_str()) {
        return false;
    }
    let verification = verification_plan(&task.markdown);
    !verification.executable_commands.is_empty()
        && verification_receipt_content_problem(
            &PathBuf::from(format!(
                "commit:{}:Auto-Verification-Receipt",
                footer.commit
            )),
            &receipt,
            &verification.executable_commands,
        )
        .is_none()
}

fn footer_task_transition_is_scoped(parent_plan: &str, plan: &str, task_id: &str) -> bool {
    let parent_tasks = parse_tasks(parent_plan);
    let tasks = parse_tasks(plan);
    let parent_matching = parent_tasks
        .iter()
        .filter(|task| task.id == task_id)
        .collect::<Vec<_>>();
    let matching = tasks
        .iter()
        .filter(|task| task.id == task_id)
        .collect::<Vec<_>>();
    let ([parent_task], [task]) = (parent_matching.as_slice(), matching.as_slice()) else {
        return false;
    };
    if task.status != TaskStatus::Done {
        return false;
    }
    let target_contract_valid = if parent_task.status == TaskStatus::Done {
        task.markdown == parent_task.markdown
    } else {
        status_neutral_task_markdown(&task.markdown)
            == status_neutral_task_markdown(&parent_task.markdown)
    };
    if !target_contract_valid {
        return false;
    }

    task_contract_counts_without(&parent_tasks, task_id)
        == task_contract_counts_without(&tasks, task_id)
}

fn task_contract_counts_without(
    tasks: &[crate::task_parser::PlanTask],
    excluded_task_id: &str,
) -> BTreeMap<String, usize> {
    let mut counts = BTreeMap::new();
    for task in tasks.iter().filter(|task| task.id != excluded_task_id) {
        *counts.entry(task.markdown.clone()).or_insert(0) += 1;
    }
    counts
}

fn status_neutral_task_markdown(markdown: &str) -> Option<String> {
    let mut lines = markdown.lines();
    let header = lines.next()?;
    let (_, body) = header.split_once("] ")?;
    let mut normalized = format!("- [?] {body}");
    for line in lines {
        normalized.push('\n');
        normalized.push_str(line);
    }
    Some(normalized)
}

fn git_command_stdout<const N: usize>(repo_root: &Path, args: [&str; N]) -> Option<String> {
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

fn git_show_file(repo_root: &Path, commit: &str, path: &str) -> Option<String> {
    let object = format!("{commit}:{path}");
    git_command_stdout(repo_root, ["show", &object])
}

#[cfg(test)]
fn shared_footer_receipt_freshness_problem(
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
        VerificationReceiptFreshnessRequest {
            expected_task_id: Some(&footer.task_id),
            expected_commands,
            declared_artifacts,
            source: VerificationReceiptSource::CommitFooter,
            limits: SourceStateLimits::default(),
        },
        None,
    ))
}

fn parse_verification_receipt_footer(
    commit: &str,
    body: &str,
) -> Option<VerificationReceiptFooter> {
    let mut task_id = None::<String>;
    let mut encoded = None::<String>;
    let mut version = None::<String>;
    for line in body.lines() {
        let trimmed = line.trim();
        if let Some(value) = trimmed.strip_prefix(RECEIPT_FOOTER_VERSION) {
            if version.replace(value.trim().to_string()).is_some() {
                return None;
            }
        } else if let Some(value) = trimmed.strip_prefix(RECEIPT_FOOTER_TASK) {
            let value = value.trim();
            if value.is_empty() || task_id.replace(value.to_string()).is_some() {
                return None;
            }
        } else if let Some(value) = trimmed.strip_prefix(RECEIPT_FOOTER_JSON) {
            let value = value.trim();
            if value.is_empty() || encoded.replace(value.to_string()).is_some() {
                return None;
            }
        }
    }
    if version.as_deref() != Some("1") {
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

fn compact_receipt_json_for_footer(
    receipt_text: &str,
    owned_inputs_fp: Option<&str>,
    verified_source_state: Option<&str>,
) -> Result<String> {
    let mut value = serde_json::from_str::<Value>(receipt_text)?;
    prune_receipt_output_tails(&mut value);
    if let Some(object) = value.as_object_mut() {
        if let Some(fp) = owned_inputs_fp {
            object.insert(
                "task_owned_inputs_v1".to_string(),
                Value::String(fp.to_string()),
            );
        }
        if let Some(fp) = verified_source_state {
            object.insert("source_state_v2".to_string(), Value::String(fp.to_string()));
        }
    }
    Ok(serde_json::to_string(&value)?)
}

/// Compute the `task-owned-inputs-v1` fingerprint for `task_id` by parsing the
/// plan currently on disk. Returns `None` (no stamp) when the plan is missing,
/// the task is absent from it, or git enumeration fails.
fn task_owned_inputs_fingerprint_for(repo_root: &Path, task_id: &str) -> Option<String> {
    let plan_text = fs::read_to_string(repo_root.join("IMPLEMENTATION_PLAN.md")).ok()?;
    let tasks = crate::task_parser::parse_tasks(&plan_text);
    super::owned_inputs::compute_task_owned_inputs_fingerprint(repo_root, task_id, &tasks)
}

/// Read the stamped `task-owned-inputs-v1` fingerprint out of a footer receipt's
/// embedded JSON. `None` when the footer predates the field (legacy) or is
/// unparseable.
pub(crate) fn footer_task_owned_inputs(footer: &VerificationReceiptFooter) -> Option<String> {
    serde_json::from_str::<VerificationReceipt>(&footer.receipt_text)
        .ok()?
        .task_owned_inputs_v1
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
    /// Legacy host-only source fingerprint. It is retained only so old footer
    /// receipts deserialize and fail closed with an explicit re-execution
    /// requirement.
    #[serde(default)]
    source_state_v1: Option<String>,
    /// Current host-only source fingerprint. Version 2 recursively binds
    /// checked-out submodule state in addition to the root index/worktree.
    #[serde(default)]
    source_state_v2: Option<String>,
    #[serde(default, alias = "completion_artifacts")]
    declared_artifacts: Vec<VerificationReceiptArtifact>,
    #[serde(default)]
    commands: Vec<VerificationReceiptCommand>,
    /// Versioned per-task owned-inputs fingerprint (`task-owned-inputs-v1`),
    /// stamped by the host into the closeout-commit footer. Absent on legacy
    /// receipts, which fall back to whole-repo drift behavior.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    task_owned_inputs_v1: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize, Eq, PartialEq)]
struct VerifiedSourceAttestation {
    version: u32,
    task_id: String,
    source_state_v2: String,
    receipt_proof_sha256: String,
    expected_commands: Vec<String>,
}

#[derive(Serialize)]
struct VerifiedReceiptProofPayload<'a> {
    task_id: &'a Option<String>,
    declared_artifacts: &'a [VerificationReceiptArtifact],
    commands: &'a [VerificationReceiptCommand],
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

struct VerificationReceiptFreshnessRequest<'a> {
    expected_task_id: Option<&'a str>,
    expected_commands: &'a [String],
    declared_artifacts: &'a [String],
    source: VerificationReceiptSource,
    limits: SourceStateLimits,
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
                    VerificationReceiptFreshnessRequest {
                        expected_task_id: Some(&footer.task_id),
                        expected_commands,
                        declared_artifacts,
                        source: VerificationReceiptSource::CommitFooter,
                        limits: SourceStateLimits::default(),
                    },
                    None,
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
    for expected_command in expected_commands {
        if let Some(problem) = verification_wrapper_binding_problem(
            verification_receipt_path,
            receipt,
            expected_command,
        ) {
            return Some(problem);
        }
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

pub(crate) fn direct_verification_receipt_freshness_problem(
    repo_root: &Path,
    verification_receipt_path: &Path,
    receipt_text: &str,
    expected_commands: &[String],
    declared_artifacts: &[String],
) -> Result<Option<String>> {
    direct_verification_receipt_freshness_problem_with_limits(
        repo_root,
        verification_receipt_path,
        receipt_text,
        expected_commands,
        declared_artifacts,
        SourceStateLimits::default(),
    )
}

fn direct_verification_receipt_freshness_problem_with_limits(
    repo_root: &Path,
    verification_receipt_path: &Path,
    receipt_text: &str,
    expected_commands: &[String],
    declared_artifacts: &[String],
    limits: SourceStateLimits,
) -> Result<Option<String>> {
    let receipt = serde_json::from_str::<VerificationReceipt>(receipt_text).with_context(|| {
        format!(
            "invalid verification receipt `{}`",
            verification_receipt_path.display()
        )
    })?;
    Ok(verification_receipt_freshness_problem_for_source(
        repo_root,
        verification_receipt_path,
        &receipt,
        VerificationReceiptFreshnessRequest {
            expected_task_id: task_id_from_receipt_path(verification_receipt_path).as_deref(),
            expected_commands,
            declared_artifacts,
            source: VerificationReceiptSource::JsonFile,
            limits,
        },
        None,
    ))
}

pub(crate) fn direct_verification_receipt_problem(
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
    if let Some(problem) = verification_receipt_freshness_problem(
        repo_root,
        verification_receipt_path,
        &receipt,
        expected_commands,
        declared_artifacts,
    ) {
        return Ok(Some(problem));
    }
    Ok(verification_receipt_content_problem(
        verification_receipt_path,
        &receipt,
        expected_commands,
    ))
}

fn direct_verification_receipt_problem_with_bounded_freshness(
    repo_root: &Path,
    verification_receipt_path: &Path,
    receipt_text: &str,
    expected_commands: &[String],
    declared_artifacts: &[String],
    bounded: &mut BoundedFreshnessContext<'_>,
) -> Result<Option<String>> {
    let receipt = serde_json::from_str::<VerificationReceipt>(receipt_text).with_context(|| {
        format!(
            "invalid verification receipt `{}`",
            verification_receipt_path.display()
        )
    })?;
    if let Some(problem) = verification_receipt_freshness_problem_for_source(
        repo_root,
        verification_receipt_path,
        &receipt,
        VerificationReceiptFreshnessRequest {
            expected_task_id: task_id_from_receipt_path(verification_receipt_path).as_deref(),
            expected_commands,
            declared_artifacts,
            source: VerificationReceiptSource::JsonFile,
            limits: bounded.limits,
        },
        Some(bounded),
    ) {
        return Ok(Some(problem));
    }
    Ok(verification_receipt_content_problem(
        verification_receipt_path,
        &receipt,
        expected_commands,
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
        VerificationReceiptFreshnessRequest {
            expected_task_id: task_id_from_receipt_path(verification_receipt_path).as_deref(),
            expected_commands,
            declared_artifacts,
            source: VerificationReceiptSource::JsonFile,
            limits: SourceStateLimits::default(),
        },
        None,
    )
}

fn verification_receipt_freshness_problem_for_source(
    repo_root: &Path,
    verification_receipt_path: &Path,
    receipt: &VerificationReceipt,
    request: VerificationReceiptFreshnessRequest<'_>,
    mut bounded: Option<&mut BoundedFreshnessContext<'_>>,
) -> Option<String> {
    let VerificationReceiptFreshnessRequest {
        expected_task_id,
        expected_commands,
        declared_artifacts,
        source,
        limits,
    } = request;

    // Establish the receipt's identity and command binding before consulting
    // repository state. These receipt-local failures are more precise than a
    // repository collection error and do not depend on Git being available.
    if let Some(expected) = expected_task_id {
        match receipt.task_id.as_deref() {
            Some(recorded) if recorded == expected => {}
            Some(recorded) => {
                return Some(format!(
                    "receipt task_id `{recorded}` does not match evidence identity `{expected}`"
                ))
            }
            None => {
                return Some(format!(
                    "receipt is missing task_id for evidence identity `{expected}`"
                ))
            }
        }
    }

    for expected_command in expected_commands {
        if let Some(problem) = verification_wrapper_binding_problem(
            verification_receipt_path,
            receipt,
            expected_command,
        ) {
            return Some(problem);
        }
    }

    let (current_commit, current_dirty_fingerprint, current_dirty_is_clean, current_plan_hash) =
        if let Some(context) = bounded.as_mut() {
            let current_commit =
                match current_git_commit_with_budget(repo_root, context.limits, context.budget) {
                    Ok(commit) => Some(commit),
                    Err(err) => {
                        return Some(format!("cannot collect bounded current Git HEAD: {err:#}"))
                    }
                };
            let dirty = match current_dirty_state_snapshot_with_budget(
                repo_root,
                context.limits,
                context.budget,
            ) {
                Ok(snapshot) => snapshot,
                Err(err) => {
                    return Some(format!(
                        "cannot collect bounded current dirty state: {err:#}"
                    ))
                }
            };
            (
                current_commit,
                Some(dirty.fingerprint),
                dirty.is_clean,
                Some(normalized_plan_hash_bytes(context.plan_input)),
            )
        } else {
            let mut budget = SourceStateBudget::default();
            let current_commit =
                match current_git_commit_with_budget(repo_root, limits, &mut budget) {
                    Ok(commit) => Some(commit),
                    Err(err) => {
                        return Some(format!("cannot collect bounded current Git HEAD: {err:#}"))
                    }
                };
            let dirty =
                match current_dirty_state_snapshot_with_budget(repo_root, limits, &mut budget) {
                    Ok(snapshot) => snapshot,
                    Err(err) => {
                        return Some(format!(
                            "cannot collect bounded current dirty state: {err:#}"
                        ))
                    }
                };
            let plan_path = repo_root.join("IMPLEMENTATION_PLAN.md");
            let current_plan_hash = match read_bounded_file_bytes(
                &plan_path,
                limits,
                &mut budget,
                "freshness IMPLEMENTATION_PLAN.md input",
            ) {
                Ok(plan) => Some(normalized_plan_hash_bytes(&plan)),
                Err(err) => {
                    return Some(format!(
                        "cannot collect bounded current plan input: {err:#}"
                    ))
                }
            };
            (
                current_commit,
                Some(dirty.fingerprint),
                dirty.is_clean,
                current_plan_hash,
            )
        };
    let mut json_receipt_commit_is_current = false;

    if source == VerificationReceiptSource::JsonFile {
        if let Some(current) = current_commit {
            match receipt.commit.as_deref() {
                Some(recorded) if recorded == current => {
                    json_receipt_commit_is_current = true;
                }
                Some(recorded) => {
                    return Some(format!(
                        "commit mismatch, recorded `{recorded}` is not current HEAD `{current}`"
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
                Some(recorded)
                    if recorded == LEGACY_CLEAN_PORCELAIN_SHA256
                        && dirty_state.entries.is_empty()
                        && current_dirty_is_clean => {}
                Some(recorded) => {
                    return Some(format!(
                        "dirty-state fingerprint mismatch, recorded `{recorded}` but current fingerprint is `{current}`"
                    ))
                }
                None if dirty_state.entries.is_empty() && current_dirty_is_clean => {}
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

    for expected_command in expected_commands {
        if let Some(problem) = verification_command_argv_problem(receipt, expected_command) {
            return Some(problem);
        }
    }

    match (source, receipt.source_state_v2.as_deref()) {
        (_, Some(recorded)) => {
            let current = if let Some(context) = bounded.as_mut() {
                if let Some(current) = &context.source_state {
                    Ok(current.clone())
                } else {
                    let current = current_source_state_fingerprint_with_budget_and_plan(
                        repo_root,
                        context.plan_input,
                        context.limits,
                        context.budget,
                    );
                    if let Ok(fingerprint) = &current {
                        context.source_state = Some(fingerprint.clone());
                    }
                    current
                }
            } else {
                current_source_state_fingerprint(repo_root)
            };
            match current {
            Ok(current) if recorded == current => {}
            Ok(current) => {
                return Some(format!(
                    "source-state fingerprint mismatch, recorded `{recorded}` but current fingerprint is `{current}`; host re-execution is required"
                ))
            }
            Err(err) => {
                return Some(format!(
                    "cannot compute current source-state fingerprint: {err:#}; host re-execution is required"
                ))
            }
            }
        }
        (VerificationReceiptSource::CommitFooter, None) => {
            let legacy = if receipt.source_state_v1.is_some() {
                "legacy source_state_v1"
            } else {
                "no source_state_v2"
            };
            return Some(
                format!(
                    "verification footer has {legacy} and is historical-only; host re-execution is required before a new Done transition"
                ),
            );
        }
        (VerificationReceiptSource::JsonFile, None) => {}
    }

    None
}

fn verification_command_argv_problem(
    receipt: &VerificationReceipt,
    expected_command: &str,
) -> Option<String> {
    let mut expected_argv = shell_split(expected_command)?;
    match parse_task_verification_wrapper_argv(&expected_argv) {
        Ok(Some((_, inner))) => expected_argv = inner,
        Ok(None) => {}
        Err(_) => {
            return Some(format!(
                "command `{expected_command}` has malformed task wrapper"
            ))
        }
    }
    let matching = receipt
        .commands
        .iter()
        .filter(|entry| verification_receipt_command_matches(entry, expected_command))
        .collect::<Vec<_>>();
    if matching.is_empty() {
        return Some(format!(
            "command `{expected_command}` is missing matching actual argv metadata"
        ));
    }
    if matching.iter().any(|entry| {
        entry.expected_argv.as_ref().is_some_and(|argv| {
            argv_matches_expected(
                argv,
                &expected_argv,
                expected_command,
                parse_task_verification_wrapper_argv(
                    &shell_split(expected_command).unwrap_or_default(),
                )
                .is_ok_and(|wrapper| wrapper.is_none()),
            )
        })
    }) {
        return None;
    }
    Some(format!(
        "command `{expected_command}` is missing matching expected argv metadata"
    ))
}

#[cfg(test)]
fn current_git_commit(repo_root: &Path) -> Option<String> {
    command_stdout(repo_root, ["rev-parse", "HEAD"]).map(|value| value.trim().to_string())
}

fn current_git_commit_with_budget(
    repo_root: &Path,
    limits: SourceStateLimits,
    budget: &mut SourceStateBudget,
) -> Result<String> {
    let output = required_git_output_bounded(
        repo_root,
        &["rev-parse", "--verify", "HEAD"],
        "current Git HEAD",
        limits,
        budget,
    )?;
    let commit = std::str::from_utf8(&output).context("current Git HEAD was not valid UTF-8")?;
    Ok(commit.trim().to_string())
}

pub(crate) fn current_dirty_state_fingerprint(repo_root: &Path) -> Option<String> {
    let mut budget = SourceStateBudget::default();
    current_dirty_state_snapshot_with_budget(repo_root, SourceStateLimits::default(), &mut budget)
        .ok()
        .map(|snapshot| snapshot.fingerprint)
}

struct DirtyStateSnapshot {
    fingerprint: String,
    is_clean: bool,
}

fn current_dirty_state_snapshot_with_budget(
    repo_root: &Path,
    limits: SourceStateLimits,
    budget: &mut SourceStateBudget,
) -> Result<DirtyStateSnapshot> {
    let status = required_git_output_bounded(
        repo_root,
        &[
            "status",
            "--porcelain=v1",
            "-z",
            "--untracked-files=all",
            "--no-renames",
            "--",
            ".",
            ":(exclude)IMPLEMENTATION_PLAN.md",
            ":(exclude)COMPLETED.md",
            ":(exclude)WORKLIST.md",
            ":(exclude)REVIEW.md",
            ":(exclude)AGENTS.md",
            ":(exclude)ARCHIVED.md",
            ":(exclude)RECEIPTS-DRIFT.md",
            ":(exclude).auto",
            ":(exclude).auto/**",
        ],
        "dirty status inventory",
        limits,
        budget,
    )?;
    let unstaged = required_git_output_bounded(
        repo_root,
        &[
            "diff",
            "--no-ext-diff",
            "--no-textconv",
            "--binary",
            "--full-index",
            "--no-color",
            "--no-renames",
            "--src-prefix=a/",
            "--dst-prefix=b/",
            "--",
            ".",
            ":(exclude)IMPLEMENTATION_PLAN.md",
            ":(exclude)COMPLETED.md",
            ":(exclude)WORKLIST.md",
            ":(exclude)REVIEW.md",
            ":(exclude)AGENTS.md",
            ":(exclude)ARCHIVED.md",
            ":(exclude)RECEIPTS-DRIFT.md",
            ":(exclude).auto",
            ":(exclude).auto/**",
        ],
        "unstaged dirty diff",
        limits,
        budget,
    )?;
    let staged = required_git_output_bounded(
        repo_root,
        &[
            "diff",
            "--cached",
            "--no-ext-diff",
            "--no-textconv",
            "--binary",
            "--full-index",
            "--no-color",
            "--no-renames",
            "--src-prefix=a/",
            "--dst-prefix=b/",
            "--",
            ".",
            ":(exclude)IMPLEMENTATION_PLAN.md",
            ":(exclude)COMPLETED.md",
            ":(exclude)WORKLIST.md",
            ":(exclude)REVIEW.md",
            ":(exclude)AGENTS.md",
            ":(exclude)ARCHIVED.md",
            ":(exclude)RECEIPTS-DRIFT.md",
            ":(exclude).auto",
            ":(exclude).auto/**",
        ],
        "staged dirty diff",
        limits,
        budget,
    )?;
    let untracked = required_git_nul_records_bounded(
        repo_root,
        &[
            "ls-files",
            "--others",
            "--exclude-standard",
            "-z",
            "--",
            ".",
            ":(exclude)IMPLEMENTATION_PLAN.md",
            ":(exclude)COMPLETED.md",
            ":(exclude)WORKLIST.md",
            ":(exclude)REVIEW.md",
            ":(exclude)AGENTS.md",
            ":(exclude)ARCHIVED.md",
            ":(exclude)RECEIPTS-DRIFT.md",
            ":(exclude).auto",
            ":(exclude).auto/**",
        ],
        "dirty untracked path inventory",
        limits,
        budget,
    )?;

    let mut hasher = Sha256::new();
    hasher.update(b"autodev-dirty-state-v2\0");
    hash_fingerprint_field(&mut hasher, b"status", &status);
    hash_fingerprint_field(&mut hasher, b"unstaged", &unstaged);
    hash_fingerprint_field(&mut hasher, b"staged", &staged);

    let mut paths = untracked;
    paths.sort();
    for path in paths {
        let state_path =
            std::str::from_utf8(&path).context("dirty-state untracked path was not UTF-8")?;
        budget.consume_entry(limits, state_path)?;
        hash_worktree_path_bounded(&mut hasher, repo_root, b"untracked", &path, limits, budget)?;
    }
    Ok(DirtyStateSnapshot {
        fingerprint: format!("{:x}", hasher.finalize()),
        is_clean: status.is_empty(),
    })
}

#[derive(Clone, Copy)]
struct SourceStateLimits {
    max_submodule_depth: usize,
    max_entries: usize,
    max_bytes: u64,
}

struct BoundedFreshnessContext<'a> {
    limits: SourceStateLimits,
    budget: &'a mut SourceStateBudget,
    plan_input: &'a [u8],
    source_state: Option<String>,
}

impl Default for SourceStateLimits {
    fn default() -> Self {
        Self {
            max_submodule_depth: SOURCE_STATE_MAX_SUBMODULE_DEPTH,
            max_entries: SOURCE_STATE_MAX_ENTRIES,
            max_bytes: SOURCE_STATE_MAX_BYTES,
        }
    }
}

#[derive(Default)]
struct SourceStateBudget {
    entries: usize,
    bytes: u64,
    visited_repositories: BTreeSet<PathBuf>,
}

impl SourceStateBudget {
    fn consume_bytes(
        &mut self,
        bytes: usize,
        limits: SourceStateLimits,
        context: &str,
    ) -> Result<()> {
        self.bytes = self
            .bytes
            .saturating_add(u64::try_from(bytes).unwrap_or(u64::MAX));
        if self.bytes > limits.max_bytes {
            bail!(
                "source-state collection exceeded the {} byte bound while reading {context}",
                limits.max_bytes
            );
        }
        Ok(())
    }

    fn consume_entry(&mut self, limits: SourceStateLimits, state_path: &str) -> Result<()> {
        self.entries = self.entries.saturating_add(1);
        if self.entries > limits.max_entries {
            bail!(
                "source-state collection exceeded the {} entry bound at `{state_path}`",
                limits.max_entries
            );
        }
        self.consume_bytes(state_path.len(), limits, "state path")
    }
}

fn current_source_state_fingerprint(repo_root: &Path) -> Result<String> {
    current_source_state_fingerprint_with_limits(repo_root, SourceStateLimits::default())
}

fn current_source_state_fingerprint_with_limits(
    repo_root: &Path,
    limits: SourceStateLimits,
) -> Result<String> {
    let mut budget = SourceStateBudget::default();
    let plan_path = repo_root.join("IMPLEMENTATION_PLAN.md");
    let plan_input = match read_bounded_file_bytes(
        &plan_path,
        limits,
        &mut budget,
        "normalized IMPLEMENTATION_PLAN.md input",
    ) {
        Ok(plan) => plan,
        Err(err)
            if err
                .downcast_ref::<std::io::Error>()
                .is_some_and(|io| io.kind() == std::io::ErrorKind::NotFound) =>
        {
            let missing = b"<missing>".to_vec();
            budget.consume_bytes(
                missing.len(),
                limits,
                "normalized IMPLEMENTATION_PLAN.md input",
            )?;
            missing
        }
        Err(err) => return Err(err),
    };
    current_source_state_fingerprint_with_budget_and_plan(
        repo_root,
        &plan_input,
        limits,
        &mut budget,
    )
}

fn current_source_state_fingerprint_with_budget_and_plan(
    repo_root: &Path,
    plan_input: &[u8],
    limits: SourceStateLimits,
    budget: &mut SourceStateBudget,
) -> Result<String> {
    let mut hasher = Sha256::new();
    hasher.update(b"autodev-source-state-v2\0");
    let plan_text =
        std::str::from_utf8(plan_input).context("IMPLEMENTATION_PLAN.md was not valid UTF-8")?;
    let plan = normalize_plan_status_markers(plan_text).into_bytes();
    hash_fingerprint_field(&mut hasher, b"normalized-plan", &plan);

    collect_repository_source_state(repo_root, "", 0, limits, budget, &mut hasher)?;
    Ok(format!("{:x}", hasher.finalize()))
}

fn collect_repository_source_state(
    repo_root: &Path,
    state_prefix: &str,
    depth: usize,
    limits: SourceStateLimits,
    budget: &mut SourceStateBudget,
    hasher: &mut Sha256,
) -> Result<()> {
    if depth > limits.max_submodule_depth {
        bail!(
            "source-state submodule nesting exceeded depth bound {} at {}",
            limits.max_submodule_depth,
            repo_root.display()
        );
    }
    let canonical_repo = fs::canonicalize(repo_root).with_context(|| {
        format!(
            "failed to canonicalize source repository {}",
            repo_root.display()
        )
    })?;
    if !budget.visited_repositories.insert(canonical_repo.clone()) {
        bail!(
            "source-state submodule cycle or duplicate canonical repository detected at {}",
            canonical_repo.display()
        );
    }

    let mut index_args = vec!["ls-files", "--stage", "-z", "--", "."];
    append_source_state_exclusion_pathspecs(&mut index_args, depth);
    let index = required_git_nul_records_bounded(
        repo_root,
        &index_args,
        "git index inventory",
        limits,
        budget,
    )?;
    let mut index_records = Vec::<(String, String, String)>::new();
    for raw_record in index {
        let record = std::str::from_utf8(&raw_record)
            .context("source-state git index entry was not UTF-8")?;
        let (metadata, path) = record
            .split_once('\t')
            .context("source-state git index entry lacked a path separator")?;
        if source_state_path_is_excluded_at_depth(path, depth) {
            continue;
        }
        let mode = metadata
            .split_whitespace()
            .next()
            .context("source-state git index entry lacked a mode")?;
        index_records.push((path.to_string(), metadata.to_string(), mode.to_string()));
    }
    index_records.sort();

    let mut index_modes = BTreeMap::<String, Vec<String>>::new();
    for (path, metadata, mode) in index_records {
        let state_path = prefixed_source_state_path(state_prefix, &path);
        budget.consume_entry(limits, &state_path)?;
        budget.consume_bytes(metadata.len(), limits, "git index metadata")?;
        hash_source_state_record(hasher, &state_path, b"index", metadata.as_bytes());
        index_modes.entry(path).or_default().push(mode);
    }

    for (path, modes) in &index_modes {
        let state_path = prefixed_source_state_path(state_prefix, path);
        budget.consume_entry(limits, &state_path)?;
        let absolute = repo_root.join(path);
        let is_gitlink = modes.iter().any(|mode| mode == "160000");
        if is_gitlink && modes.iter().any(|mode| mode != "160000") {
            bail!("source-state index has mixed gitlink and file modes at `{state_path}`");
        }
        if is_gitlink {
            collect_checked_out_submodule_state(
                &absolute,
                &state_path,
                depth,
                limits,
                budget,
                hasher,
            )?;
        } else {
            hash_source_worktree_path(&absolute, &state_path, b"worktree", limits, budget, hasher)?;
        }
    }

    let mut ignored_control_args = vec![
        "ls-files",
        "--others",
        "--ignored",
        "--exclude-per-directory=.gitignore",
        "-z",
        "--",
        ":(top,glob).gitignore",
        ":(top,glob)**/.gitignore",
    ];
    append_source_state_exclusion_pathspecs(&mut ignored_control_args, depth);
    let ignored_controls = required_git_nul_records_bounded(
        repo_root,
        &ignored_control_args,
        "ignored untracked .gitignore inventory",
        limits,
        budget,
    )?;
    if let Some(path) = ignored_controls.first() {
        bail!(
            "source-state collection found untracked ignore control `{}`; .gitignore files must be tracked before they can exclude source",
            String::from_utf8_lossy(path)
        );
    }

    let mut untracked_args = vec![
        "ls-files",
        "--others",
        "--exclude-per-directory=.gitignore",
        "-z",
        "--",
        ".",
    ];
    append_source_state_exclusion_pathspecs(&mut untracked_args, depth);
    let untracked = required_git_nul_records_bounded(
        repo_root,
        &untracked_args,
        "untracked path inventory",
        limits,
        budget,
    )?;
    let mut untracked_paths = untracked
        .into_iter()
        .map(|path| {
            std::str::from_utf8(&path)
                .context("source-state untracked path was not UTF-8")
                .map(str::to_string)
        })
        .collect::<Result<Vec<_>>>()?;
    untracked_paths.sort();
    for path in untracked_paths {
        if source_state_path_is_excluded_at_depth(&path, depth) {
            continue;
        }
        if source_state_path_is_gitignore(&path) {
            bail!(
                "source-state collection found untracked ignore control `{path}`; .gitignore files must be tracked before they can exclude source"
            );
        }
        let state_path = prefixed_source_state_path(state_prefix, &path);
        budget.consume_entry(limits, &state_path)?;
        hash_source_worktree_path(
            &repo_root.join(&path),
            &state_path,
            b"untracked",
            limits,
            budget,
            hasher,
        )?;
    }
    Ok(())
}

fn collect_checked_out_submodule_state(
    submodule_root: &Path,
    state_path: &str,
    parent_depth: usize,
    limits: SourceStateLimits,
    budget: &mut SourceStateBudget,
    hasher: &mut Sha256,
) -> Result<()> {
    let metadata = fs::symlink_metadata(submodule_root).with_context(|| {
        format!("source-state gitlink `{state_path}` is not checked out as a repository")
    })?;
    if !metadata.is_dir() || !submodule_root.join(".git").exists() {
        bail!("source-state gitlink `{state_path}` is not an initialized submodule");
    }
    let canonical_root = fs::canonicalize(submodule_root)
        .with_context(|| format!("failed to canonicalize checked-out submodule `{state_path}`"))?;
    let top_level = required_git_output_bounded(
        submodule_root,
        &["rev-parse", "--show-toplevel"],
        "submodule top-level",
        limits,
        budget,
    )?;
    let top_level_text =
        std::str::from_utf8(&top_level).context("submodule top-level was not UTF-8")?;
    let canonical_top_level = fs::canonicalize(top_level_text.trim()).with_context(|| {
        format!("failed to canonicalize Git top-level for submodule `{state_path}`")
    })?;
    if canonical_top_level != canonical_root {
        bail!(
            "source-state gitlink `{state_path}` resolved to unexpected repository {}",
            canonical_top_level.display()
        );
    }
    let head = required_git_output_bounded(
        submodule_root,
        &["rev-parse", "--verify", "HEAD"],
        "submodule HEAD",
        limits,
        budget,
    )?;
    hash_source_state_record(
        hasher,
        state_path,
        b"worktree-submodule-head",
        head.trim_ascii(),
    );
    collect_repository_source_state(
        submodule_root,
        state_path,
        parent_depth + 1,
        limits,
        budget,
        hasher,
    )
    .with_context(|| format!("failed to capture recursive source state for `{state_path}`"))
}

fn hash_source_worktree_path(
    absolute: &Path,
    state_path: &str,
    namespace: &[u8],
    limits: SourceStateLimits,
    budget: &mut SourceStateBudget,
    hasher: &mut Sha256,
) -> Result<()> {
    let metadata = match fs::symlink_metadata(absolute) {
        Ok(metadata) => metadata,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            hash_source_state_record(hasher, state_path, namespace, b"missing");
            return Ok(());
        }
        Err(err) => {
            return Err(err)
                .with_context(|| format!("failed to inspect source-state path `{state_path}`"));
        }
    };
    let mode = filesystem_mode(&metadata).to_be_bytes();
    hash_source_state_record(hasher, state_path, b"filesystem-mode", &mode);
    if metadata.file_type().is_symlink() {
        let target = fs::read_link(absolute)
            .with_context(|| format!("failed to read source-state symlink `{state_path}`"))?;
        let target_bytes = target.as_os_str().as_encoded_bytes();
        budget.consume_bytes(target_bytes.len(), limits, "symlink target")?;
        hash_source_state_record(hasher, state_path, namespace, b"symlink");
        hash_source_state_record(hasher, state_path, b"symlink-target", target_bytes);
        return Ok(());
    }
    if !metadata.is_file() {
        bail!("unsupported source-state filesystem type at `{state_path}`");
    }
    let content_hash =
        bounded_source_file_hash(absolute, state_path, metadata.len(), limits, budget)?;
    hash_source_state_record(hasher, state_path, namespace, b"file");
    hash_source_state_record(hasher, state_path, b"content-sha256", &content_hash);
    Ok(())
}

fn bounded_source_file_hash(
    path: &Path,
    state_path: &str,
    advertised_len: u64,
    limits: SourceStateLimits,
    budget: &mut SourceStateBudget,
) -> Result<sha2::digest::Output<Sha256>> {
    let remaining = limits.max_bytes.saturating_sub(budget.bytes);
    if advertised_len > remaining {
        bail!(
            "source-state collection exceeded the {} byte bound while reading `{state_path}`",
            limits.max_bytes
        );
    }
    let mut file = fs::File::open(path)
        .with_context(|| format!("failed to open source-state file `{state_path}`"))?;
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = file
            .read(&mut buffer)
            .with_context(|| format!("failed to read source-state file `{state_path}`"))?;
        if read == 0 {
            break;
        }
        budget.consume_bytes(read, limits, &format!("source-state file `{state_path}`"))?;
        hasher.update(&buffer[..read]);
    }
    Ok(hasher.finalize())
}

fn hash_source_state_record(hasher: &mut Sha256, state_path: &str, namespace: &[u8], value: &[u8]) {
    hasher.update(b"source-state-record\0");
    hash_fingerprint_field(hasher, b"path", state_path.as_bytes());
    hash_fingerprint_field(hasher, b"namespace", namespace);
    hash_fingerprint_field(hasher, b"value", value);
}

fn prefixed_source_state_path(prefix: &str, path: &str) -> String {
    if prefix.is_empty() {
        path.to_string()
    } else {
        format!("{prefix}/{path}")
    }
}

fn source_state_path_is_excluded_at_depth(relative_path: &str, depth: usize) -> bool {
    relative_path == ".auto"
        || relative_path.starts_with(".auto/")
        || relative_path == ".git"
        || relative_path.starts_with(".git/")
        || (depth == 0 && HOST_QUEUE_STATE_FILES.contains(&relative_path))
}

fn source_state_path_is_gitignore(relative_path: &str) -> bool {
    relative_path == ".gitignore" || relative_path.ends_with("/.gitignore")
}

fn append_source_state_exclusion_pathspecs(args: &mut Vec<&str>, depth: usize) {
    args.extend([
        ":(top,exclude).auto",
        ":(top,exclude).auto/**",
        ":(top,exclude).git",
        ":(top,exclude).git/**",
    ]);
    if depth == 0 {
        args.extend([
            ":(top,exclude)IMPLEMENTATION_PLAN.md",
            ":(top,exclude)COMPLETED.md",
            ":(top,exclude)WORKLIST.md",
            ":(top,exclude)REVIEW.md",
            ":(top,exclude)AGENTS.md",
            ":(top,exclude)ARCHIVED.md",
            ":(top,exclude)RECEIPTS-DRIFT.md",
        ]);
    }
}

fn read_bounded_file_bytes(
    path: &Path,
    limits: SourceStateLimits,
    budget: &mut SourceStateBudget,
    context: &str,
) -> Result<Vec<u8>> {
    let mut file = fs::File::open(path)
        .with_context(|| format!("failed to open {context} at {}", path.display()))?;
    let advertised_len = file
        .metadata()
        .with_context(|| format!("failed to inspect {context} at {}", path.display()))?
        .len();
    if advertised_len > limits.max_bytes.saturating_sub(budget.bytes) {
        bail!(
            "source-state collection exceeded the {} byte bound while reading {context}",
            limits.max_bytes
        );
    }
    read_bounded_bytes(&mut file, limits, budget, context)
}

fn read_bounded_utf8_file(
    path: &Path,
    limits: SourceStateLimits,
    budget: &mut SourceStateBudget,
    context: &str,
) -> Result<String> {
    let bytes = read_bounded_file_bytes(path, limits, budget, context)?;
    String::from_utf8(bytes).with_context(|| format!("{context} was not valid UTF-8"))
}

fn read_bounded_bytes(
    reader: &mut impl Read,
    limits: SourceStateLimits,
    budget: &mut SourceStateBudget,
    context: &str,
) -> Result<Vec<u8>> {
    let mut output = Vec::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let remaining = limits.max_bytes.saturating_sub(budget.bytes);
        let read_bound = if remaining == 0 {
            1
        } else {
            usize::try_from(remaining.min(u64::try_from(buffer.len()).unwrap_or(u64::MAX)))
                .unwrap_or(buffer.len())
        };
        let read = reader
            .read(&mut buffer[..read_bound])
            .with_context(|| format!("failed while reading {context}"))?;
        if read == 0 {
            break;
        }
        budget.consume_bytes(read, limits, context)?;
        output.extend_from_slice(&buffer[..read]);
    }
    Ok(output)
}

fn read_bounded_nul_records(
    reader: &mut impl Read,
    limits: SourceStateLimits,
    budget: &mut SourceStateBudget,
    context: &str,
) -> Result<Vec<Vec<u8>>> {
    let remaining_entries = limits.max_entries.saturating_sub(budget.entries);
    let mut records = Vec::new();
    let mut pending = Vec::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let remaining = limits.max_bytes.saturating_sub(budget.bytes);
        let read_bound = if remaining == 0 {
            1
        } else {
            usize::try_from(remaining.min(u64::try_from(buffer.len()).unwrap_or(u64::MAX)))
                .unwrap_or(buffer.len())
        };
        let read = reader
            .read(&mut buffer[..read_bound])
            .with_context(|| format!("failed while reading {context}"))?;
        if read == 0 {
            break;
        }
        budget.consume_bytes(read, limits, context)?;
        for byte in &buffer[..read] {
            if *byte == 0 {
                if pending.is_empty() {
                    continue;
                }
                if records.len() >= remaining_entries {
                    bail!(
                        "source-state collection exceeded the {} entry bound while reading {context}",
                        limits.max_entries
                    );
                }
                records.push(std::mem::take(&mut pending));
            } else {
                pending.push(*byte);
            }
        }
    }
    if !pending.is_empty() {
        bail!("git returned an unterminated NUL record while reading {context}");
    }
    Ok(records)
}

fn required_git_output_bounded(
    repo_root: &Path,
    args: &[&str],
    context: &str,
    limits: SourceStateLimits,
    budget: &mut SourceStateBudget,
) -> Result<Vec<u8>> {
    let mut child = Command::new("git")
        .arg("-C")
        .arg(repo_root)
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .with_context(|| format!("failed to launch git while reading {context}"))?;
    let output = child
        .stdout
        .as_mut()
        .context("git stdout pipe was unavailable")
        .and_then(|stdout| read_bounded_bytes(stdout, limits, budget, context));
    if output.is_err() {
        let _ = child.kill();
    }
    let status = child
        .wait()
        .with_context(|| format!("failed to wait for git while reading {context}"))?;
    let output = output?;
    if !status.success() {
        bail!(
            "git failed with status {status} while reading {context} in {}",
            repo_root.display(),
        );
    }
    Ok(output)
}

fn required_git_nul_records_bounded(
    repo_root: &Path,
    args: &[&str],
    context: &str,
    limits: SourceStateLimits,
    budget: &mut SourceStateBudget,
) -> Result<Vec<Vec<u8>>> {
    let mut child = Command::new("git")
        .arg("-C")
        .arg(repo_root)
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .with_context(|| format!("failed to launch git while reading {context}"))?;
    let records = child
        .stdout
        .as_mut()
        .context("git stdout pipe was unavailable")
        .and_then(|stdout| read_bounded_nul_records(stdout, limits, budget, context));
    if records.is_err() {
        let _ = child.kill();
    }
    let status = child
        .wait()
        .with_context(|| format!("failed to wait for git while reading {context}"))?;
    let records = records?;
    if !status.success() {
        bail!(
            "git failed with status {status} while reading {context} in {}",
            repo_root.display(),
        );
    }
    Ok(records)
}

fn verification_proof_payload_sha256(receipt: &VerificationReceipt) -> Result<String> {
    let payload = VerifiedReceiptProofPayload {
        task_id: &receipt.task_id,
        declared_artifacts: &receipt.declared_artifacts,
        commands: &receipt.commands,
    };
    let encoded =
        serde_json::to_vec(&payload).context("failed to serialize verification proof payload")?;
    Ok(sha256_hex(&encoded))
}

fn require_current_source_attestation_for_footer(
    repo_root: &Path,
    task_id: &str,
    receipt_path: &Path,
    receipt: &VerificationReceipt,
    expected_commands: &[String],
    source_context: &mut BoundedFreshnessContext<'_>,
) -> Result<String> {
    let attestation_path = verified_source_attestation_path(repo_root, task_id);
    let attestation_text = read_bounded_utf8_file(
        &attestation_path,
        source_context.limits,
        source_context.budget,
        "footer verified-source attestation input",
    )
    .with_context(|| {
            format!(
                "cannot prepare durable verification footer from `{}` without host verified-source attestation `{}`; host re-execution is required",
                receipt_path.display(),
                attestation_path.display()
            )
        })?;
    let attestation = serde_json::from_str::<VerifiedSourceAttestation>(&attestation_text)
        .with_context(|| {
            format!(
                "invalid host verified-source attestation `{}`",
                attestation_path.display()
            )
        })?;
    if attestation.version != VERIFIED_SOURCE_ATTESTATION_VERSION {
        bail!(
            "cannot prepare durable verification footer from `{}`: host attestation version {} is unsupported; host re-execution is required",
            receipt_path.display(),
            attestation.version
        );
    }
    if attestation.task_id != task_id {
        bail!(
            "cannot prepare durable verification footer from `{}`: host attestation task `{}` does not match `{task_id}`; host re-execution is required",
            receipt_path.display(),
            attestation.task_id
        );
    }
    let proof_sha256 = verification_proof_payload_sha256(receipt)?;
    if attestation.receipt_proof_sha256 != proof_sha256 {
        bail!(
            "cannot prepare durable verification footer from `{}`: verification proof payload changed after host verification; host re-execution is required",
            receipt_path.display()
        );
    }
    if attestation.expected_commands != expected_commands {
        bail!(
            "cannot prepare durable verification footer from `{}`: verification command set changed after host verification; host re-execution is required",
            receipt_path.display()
        );
    }
    let current = current_source_state_fingerprint_with_budget_and_plan(
        repo_root,
        source_context.plan_input,
        source_context.limits,
        source_context.budget,
    )
    .with_context(|| {
            format!(
                "cannot compute current source-state fingerprint for `{}`; host re-execution is required",
                receipt_path.display()
            )
        })?;
    if attestation.source_state_v2 != current {
        bail!(
            "cannot prepare durable verification footer from `{}`: source-state fingerprint mismatch, recorded `{}` but current fingerprint is `{current}`; host re-execution is required",
            receipt_path.display(),
            attestation.source_state_v2
        );
    }
    Ok(attestation.source_state_v2)
}

fn hash_fingerprint_field(hasher: &mut Sha256, label: &[u8], value: &[u8]) {
    hasher.update((label.len() as u64).to_be_bytes());
    hasher.update(label);
    hasher.update((value.len() as u64).to_be_bytes());
    hasher.update(value);
}

fn hash_worktree_path_bounded(
    hasher: &mut Sha256,
    repo_root: &Path,
    namespace: &[u8],
    relative_path: &[u8],
    limits: SourceStateLimits,
    budget: &mut SourceStateBudget,
) -> Result<()> {
    let relative_text =
        std::str::from_utf8(relative_path).context("dirty-state path was not valid UTF-8")?;
    let path = repo_root.join(relative_text);
    let (mode, kind, content_hash) = match fs::symlink_metadata(&path) {
        Ok(metadata) => {
            let mode = filesystem_mode(&metadata).to_be_bytes();
            if metadata.file_type().is_symlink() {
                let target = fs::read_link(&path).with_context(|| {
                    format!("failed to read dirty-state symlink `{relative_text}`")
                })?;
                let rendered = target.as_os_str().to_string_lossy();
                budget.consume_bytes(
                    rendered.len(),
                    limits,
                    &format!("dirty-state symlink `{relative_text}`"),
                )?;
                (
                    mode,
                    b"symlink".as_slice(),
                    Sha256::digest(rendered.as_bytes()),
                )
            } else if metadata.is_file() {
                (
                    mode,
                    b"file".as_slice(),
                    bounded_source_file_hash(&path, relative_text, metadata.len(), limits, budget)?,
                )
            } else {
                (mode, b"other".as_slice(), Sha256::digest([]))
            }
        }
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => (
            0_u32.to_be_bytes(),
            b"missing".as_slice(),
            Sha256::digest([]),
        ),
        Err(err) => {
            return Err(err)
                .with_context(|| format!("failed to inspect dirty-state path `{relative_text}`"));
        }
    };

    hash_fingerprint_field(hasher, b"namespace", namespace);
    hash_fingerprint_field(hasher, b"path", relative_path);
    hash_fingerprint_field(hasher, b"mode", &mode);
    hash_fingerprint_field(hasher, b"kind", kind);
    hash_fingerprint_field(hasher, b"content-sha256", &content_hash);
    Ok(())
}

#[cfg(unix)]
fn filesystem_mode(metadata: &fs::Metadata) -> u32 {
    use std::os::unix::fs::PermissionsExt;

    metadata.permissions().mode() & 0o7777
}

#[cfg(not(unix))]
fn filesystem_mode(metadata: &fs::Metadata) -> u32 {
    u32::from(metadata.permissions().readonly())
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

#[cfg(test)]
fn current_plan_hash(repo_root: &Path) -> Option<String> {
    fs::read(repo_root.join("IMPLEMENTATION_PLAN.md"))
        .ok()
        .map(|bytes| normalized_plan_hash_bytes(&bytes))
}

#[cfg(test)]
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
    let program = argv.iter().find(|tok| !is_env_assignment(tok)).map(|tok| {
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
    let mut expected_argv = match shell_split(expected_command) {
        Some(argv) => argv,
        None => return false,
    };
    let expected_wrapper_task = match parse_task_verification_wrapper_argv(&expected_argv) {
        Ok(Some((task_id, inner))) => {
            expected_argv = inner;
            Some(task_id)
        }
        Ok(None) => None,
        Err(_) => return false,
    };
    if entry.argv.is_empty() {
        return false;
    }
    let mut actual_candidate = entry.argv.clone();
    if let Ok(Some((candidate_task, inner))) =
        parse_task_verification_wrapper_argv(&actual_candidate)
    {
        if expected_wrapper_task.as_deref() != Some(candidate_task.as_str()) {
            return false;
        }
        actual_candidate = inner;
    }
    if !argv_matches_expected(
        &actual_candidate,
        &expected_argv,
        expected_command,
        expected_wrapper_task.is_none(),
    ) {
        return false;
    }

    if entry.command == expected_command {
        return true;
    }
    let Some(mut command_candidate) = shell_split(&entry.command) else {
        return false;
    };
    if let Ok(Some((candidate_task, inner))) =
        parse_task_verification_wrapper_argv(&command_candidate)
    {
        if expected_wrapper_task.as_deref() != Some(candidate_task.as_str()) {
            return false;
        }
        command_candidate = inner;
    }
    argv_matches_expected(
        &command_candidate,
        &expected_argv,
        expected_command,
        expected_wrapper_task.is_none(),
    )
}

fn argv_matches_expected(
    candidate: &[String],
    expected_argv: &[String],
    expected_command: &str,
    launcher_compatibility_allowed: bool,
) -> bool {
    if candidate == expected_argv {
        return true;
    }
    if !launcher_compatibility_allowed {
        return false;
    }

    if candidate.first().map(String::as_str) == Some("env")
        && candidate.get(1..) == Some(expected_argv)
    {
        return true;
    }

    let shell_launcher = matches!(
        candidate.first().map(String::as_str),
        Some("bash" | "sh" | "zsh" | "dash")
    );
    shell_launcher
        && candidate.len() == 3
        && matches!(candidate[1].as_str(), "-c" | "-lc" | "-cl")
        && candidate[2] == expected_command
        && shell_split(&candidate[2]).is_some_and(|inner| inner == expected_argv)
}

fn unwrap_launcher_argv(argv: &[String]) -> Option<Vec<String>> {
    let arg0 = argv.first()?.as_str();
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

fn parse_task_verification_wrapper_argv(
    argv: &[String],
) -> Result<Option<(String, Vec<String>)>, &'static str> {
    let Some(arg0) = argv.first() else {
        return Ok(None);
    };
    let wrapper = Path::new(arg0)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or(arg0.as_str());
    if wrapper != "run-task-verification.sh" {
        return Ok(None);
    }
    if arg0 != "scripts/run-task-verification.sh" {
        return Err("wrapper path must be exactly scripts/run-task-verification.sh");
    }
    if argv.len() < 4 || argv[2] != "--" {
        return Err("expected scripts/run-task-verification.sh TASK_ID -- COMMAND...");
    }
    let task_id = argv[1].trim();
    if task_id.is_empty() || task_id == "--" {
        return Err("wrapper task id must be non-empty");
    }
    let inner = argv[3..].to_vec();
    if inner.is_empty() {
        return Err("wrapper inner command must be non-empty");
    }
    Ok(Some((task_id.to_string(), inner)))
}

fn verification_wrapper_binding_problem(
    verification_receipt_path: &Path,
    receipt: &VerificationReceipt,
    expected_command: &str,
) -> Option<String> {
    let argv = shell_split(expected_command)?;
    let (expected_task_id, _) = match parse_task_verification_wrapper_argv(&argv) {
        Ok(Some(parts)) => parts,
        Ok(None) => return None,
        Err(reason) => {
            return Some(format!(
                "command `{expected_command}` has malformed task verification wrapper: {reason}"
            ))
        }
    };
    match receipt.task_id.as_deref() {
        Some(recorded) if recorded == expected_task_id => {}
        Some(recorded) => {
            return Some(format!(
                "command `{expected_command}` names wrapper task `{expected_task_id}`, but receipt task_id is `{recorded}`"
            ))
        }
        None => {
            return Some(format!(
                "command `{expected_command}` names wrapper task `{expected_task_id}`, but receipt has no task_id"
            ))
        }
    }
    if verification_receipt_path
        .extension()
        .and_then(|ext| ext.to_str())
        == Some("json")
    {
        let path_task_id = verification_receipt_path
            .file_stem()
            .and_then(|stem| stem.to_str())
            .unwrap_or_default();
        if path_task_id != expected_task_id {
            return Some(format!(
                "command `{expected_command}` names wrapper task `{expected_task_id}`, but receipt path is for `{path_task_id}`"
            ));
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::PathBuf;
    use std::process::Command;

    use base64::Engine;
    use sha2::Digest;

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

    fn git_ok<const N: usize>(root: &std::path::Path, args: [&str; N]) {
        let output = Command::new("git")
            .arg("-C")
            .arg(root)
            .args(args)
            .output()
            .expect("git command failed to launch");
        assert!(
            output.status.success(),
            "git command failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    fn git_stdout<const N: usize>(root: &std::path::Path, args: [&str; N]) -> String {
        let output = Command::new("git")
            .arg("-C")
            .arg(root)
            .args(args)
            .output()
            .expect("git command failed to launch");
        assert!(
            output.status.success(),
            "git command failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout)
            .expect("git output should be UTF-8")
            .trim()
            .to_string()
    }

    #[test]
    fn dirty_state_fingerprint_changes_when_dirty_contents_change_without_status_shape_change() {
        let root = temp_dir("content-sensitive-dirty-fingerprint");
        init_git_repo(&root);
        fs::write(root.join("source.rs"), "pub fn value() -> u8 { 0 }\n")
            .expect("failed to write tracked source");
        Command::new("git")
            .arg("-C")
            .arg(&root)
            .args(["add", "source.rs"])
            .output()
            .expect("git add source failed");
        Command::new("git")
            .arg("-C")
            .arg(&root)
            .args(["commit", "-m", "add source"])
            .output()
            .expect("git commit source failed");

        fs::write(root.join("source.rs"), "pub fn value() -> u8 { 1 }\n")
            .expect("failed to write first unstaged content");
        let unstaged_one =
            super::current_dirty_state_fingerprint(&root).expect("unstaged fingerprint");
        fs::write(root.join("source.rs"), "pub fn value() -> u8 { 2 }\n")
            .expect("failed to write second unstaged content");
        let unstaged_two =
            super::current_dirty_state_fingerprint(&root).expect("unstaged fingerprint");
        assert_ne!(
            unstaged_one, unstaged_two,
            "unstaged diff contents must contribute to the fingerprint"
        );

        fs::write(root.join("source.rs"), "pub fn value() -> u8 { 3 }\n")
            .expect("failed to write first staged content");
        Command::new("git")
            .arg("-C")
            .arg(&root)
            .args(["add", "source.rs"])
            .output()
            .expect("git add first staged content failed");
        let staged_one = super::current_dirty_state_fingerprint(&root).expect("staged fingerprint");
        fs::write(root.join("source.rs"), "pub fn value() -> u8 { 4 }\n")
            .expect("failed to write second staged content");
        Command::new("git")
            .arg("-C")
            .arg(&root)
            .args(["add", "source.rs"])
            .output()
            .expect("git add second staged content failed");
        let staged_two = super::current_dirty_state_fingerprint(&root).expect("staged fingerprint");
        assert_ne!(
            staged_one, staged_two,
            "staged diff contents must contribute to the fingerprint"
        );

        fs::write(root.join("untracked.txt"), "first\n")
            .expect("failed to write first untracked content");
        let untracked_one =
            super::current_dirty_state_fingerprint(&root).expect("untracked fingerprint");
        fs::write(root.join("untracked.txt"), "second\n")
            .expect("failed to write second untracked content");
        let untracked_two =
            super::current_dirty_state_fingerprint(&root).expect("untracked fingerprint");
        assert_ne!(
            untracked_one, untracked_two,
            "untracked file contents must contribute to the fingerprint"
        );
    }

    #[test]
    fn dirty_state_fingerprint_excludes_host_queue_and_auto_runtime_churn() {
        let root = temp_dir("queue-stable-dirty-fingerprint");
        init_git_repo(&root);
        let before = super::current_dirty_state_fingerprint(&root).expect("baseline fingerprint");

        fs::write(
            root.join("IMPLEMENTATION_PLAN.md"),
            "- [x] `TASK-QUEUE` Queue-only mutation\n",
        )
        .expect("failed to update plan");
        fs::write(root.join("REVIEW.md"), "# REVIEW\n\nqueue update\n")
            .expect("failed to update review");
        fs::create_dir_all(root.join(".auto/parallel")).expect("failed to create runtime dir");
        fs::write(root.join(".auto/parallel/runtime.json"), "{}\n")
            .expect("failed to write runtime state");

        let after =
            super::current_dirty_state_fingerprint(&root).expect("queue-mutated fingerprint");
        assert_eq!(
            before, after,
            "host queue and .auto runtime churn must not invalidate verified source"
        );
    }

    #[test]
    fn receipt_wrappers_emit_the_same_content_sensitive_dirty_fingerprint_as_rust() {
        let root = temp_dir("wrapper-dirty-fingerprint-parity");
        init_git_repo(&root);
        fs::write(
            root.join("IMPLEMENTATION_PLAN.md"),
            "- [~] `TASK-PARITY` Fingerprint parity\n  Verification:\n    - `true`\n  Completion artifacts: none\n  Dependencies: none\n",
        )
        .expect("failed to write parity plan");
        fs::write(root.join("source.txt"), "committed\n").expect("failed to write source");
        Command::new("git")
            .arg("-C")
            .arg(&root)
            .args(["add", "IMPLEMENTATION_PLAN.md", "source.txt"])
            .output()
            .expect("git add parity fixture failed");
        Command::new("git")
            .arg("-C")
            .arg(&root)
            .args(["commit", "-m", "parity fixture"])
            .output()
            .expect("git commit parity fixture failed");
        fs::write(root.join("source.txt"), "unstaged\n").expect("failed to dirty source");
        fs::write(root.join("untracked.txt"), "untracked\n")
            .expect("failed to write untracked source");
        let expected = super::current_dirty_state_fingerprint(&root).expect("Rust fingerprint");

        let standalone =
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("scripts/verification_receipt.py");
        let output = Command::new("python3")
            .current_dir(&root)
            .arg(&standalone)
            .args(["record", "--argv", "true", "TASK-PARITY", "true", "0"])
            .output()
            .expect("failed to run standalone receipt recorder");
        assert!(
            output.status.success(),
            "standalone recorder failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let receipt_path = root.join(".auto/symphony/verification-receipts/TASK-PARITY.json");
        let standalone_receipt: serde_json::Value = serde_json::from_slice(
            &fs::read(&receipt_path).expect("failed to read standalone receipt"),
        )
        .expect("failed to parse standalone receipt");
        assert_eq!(
            standalone_receipt["dirty_state"]["fingerprint"].as_str(),
            Some(expected.as_str())
        );

        let wrapper =
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("scripts/run-task-verification.sh");
        let output = Command::new(&wrapper)
            .current_dir(&root)
            .args(["TASK-PARITY", "--", "true"])
            .output()
            .expect("failed to run task verification wrapper");
        assert!(
            output.status.success(),
            "task wrapper failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let wrapper_receipt: serde_json::Value = serde_json::from_slice(
            &fs::read(&receipt_path).expect("failed to read wrapper receipt"),
        )
        .expect("failed to parse wrapper receipt");
        assert_eq!(
            wrapper_receipt["dirty_state"]["fingerprint"].as_str(),
            Some(expected.as_str())
        );
    }

    fn write_source_bound_receipt(root: &std::path::Path, task_id: &str) {
        fs::create_dir_all(root.join(".auto/symphony/verification-receipts"))
            .expect("failed to create receipts directory");
        let commit = super::current_git_commit(root).expect("current commit");
        let dirty_state =
            super::current_dirty_state_fingerprint(root).expect("dirty-state fingerprint");
        let plan_hash = super::current_plan_hash(root).expect("plan hash");
        fs::write(
            root.join(format!(
                ".auto/symphony/verification-receipts/{task_id}.json"
            )),
            format!(
                r#"{{"task_id":"{task_id}","commit":"{commit}","dirty_state":{{"fingerprint":"{dirty_state}"}},"plan_hash":"{plan_hash}","commands":[{{"command":"cargo test source_binding","argv":["cargo","test","source_binding"],"expected_argv":["cargo","test","source_binding"],"exit_code":0,"status":"passed"}}]}}"#
            ),
        )
        .expect("failed to write source-bound receipt");
    }

    fn source_bound_footer_fixture(root: &std::path::Path, task_id: &str) {
        write_source_bound_receipt(root, task_id);
        super::record_verified_source_attestation(root, task_id)
            .expect("host should attest freshly verified source");
    }

    fn source_bound_root_footer_fixture(name: &str) -> PathBuf {
        let root = temp_dir(name);
        init_git_repo(&root);
        fs::create_dir_all(root.join("src")).expect("create root source directory");
        fs::write(root.join("src/lib.rs"), "pub fn verified() {}\n").expect("write root source");
        fs::write(root.join(".gitignore"), "build-cache/\n")
            .expect("write tracked source exclusions");
        fs::write(
            root.join("IMPLEMENTATION_PLAN.md"),
            "- [~] `TASK-SOURCE` Source-bound proof\n  Owns: `src/lib.rs`\n  Verification:\n    - `cargo test source_binding`\n  Completion artifacts: none\n  Dependencies: none\n",
        )
        .expect("write partial root plan");
        fs::write(
            root.join("REVIEW.md"),
            "# REVIEW\n\nAwaiting closeout for TASK-SOURCE\n",
        )
        .expect("write root review");
        git_ok(
            &root,
            [
                "add",
                "src/lib.rs",
                ".gitignore",
                "IMPLEMENTATION_PLAN.md",
                "REVIEW.md",
            ],
        );
        git_ok(&root, ["commit", "-m", "seed source-bound root"]);
        source_bound_footer_fixture(&root, "TASK-SOURCE");
        fs::write(
            root.join("IMPLEMENTATION_PLAN.md"),
            "- [x] `TASK-SOURCE` Source-bound proof\n  Owns: `src/lib.rs`\n  Verification:\n    - `cargo test source_binding`\n  Completion artifacts: none\n  Dependencies: none\n",
        )
        .expect("mark root task done");
        fs::write(
            root.join("REVIEW.md"),
            "# REVIEW\n\n## `TASK-SOURCE`\n\nIndependent review: CLEAR\n",
        )
        .expect("clear root review");
        root
    }

    #[test]
    fn footer_generation_rejects_untracked_source_hidden_by_git_info_exclude() {
        let root = source_bound_root_footer_fixture("source-bound-info-exclude");
        fs::write(root.join(".git/info/exclude"), "hidden-source.rs\n")
            .expect("install hostile repository exclude");
        fs::write(root.join("hidden-source.rs"), "unverified source\n")
            .expect("write hidden untracked source");

        let error = super::verification_receipt_commit_footer(&root, "TASK-SOURCE")
            .expect_err(".git/info/exclude must not hide untracked source from closeout");
        assert!(format!("{error:#}").contains("source-state"), "{error:#}");

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn footer_generation_rejects_untracked_source_hidden_by_core_excludes_file() {
        let root = source_bound_root_footer_fixture("source-bound-core-excludes");
        let excludes_root = temp_dir("source-bound-global-excludes");
        let excludes_file = excludes_root.join("hostile-excludes");
        fs::write(&excludes_file, "hidden-source.rs\n").expect("write hostile global excludes");
        let excludes_path = excludes_file.to_string_lossy().into_owned();
        git_ok(&root, ["config", "core.excludesFile", &excludes_path]);
        fs::write(root.join("hidden-source.rs"), "unverified source\n")
            .expect("write globally hidden untracked source");

        let error = super::verification_receipt_commit_footer(&root, "TASK-SOURCE")
            .expect_err("core.excludesFile must not hide untracked source from closeout");
        assert!(format!("{error:#}").contains("source-state"), "{error:#}");

        let _ = fs::remove_dir_all(root);
        let _ = fs::remove_dir_all(excludes_root);
    }

    #[test]
    fn footer_generation_rejects_untracked_gitignore_that_hides_source() {
        let root = source_bound_root_footer_fixture("source-bound-untracked-gitignore");
        fs::create_dir_all(root.join("untracked-policy")).expect("create untracked policy");
        fs::write(root.join("untracked-policy/.gitignore"), "*\n")
            .expect("write untracked ignore control");
        fs::write(
            root.join("untracked-policy/hidden-source.rs"),
            "unverified source\n",
        )
        .expect("write source hidden by untracked ignore control");

        let error = super::verification_receipt_commit_footer(&root, "TASK-SOURCE")
            .expect_err("an untracked .gitignore must not gain source-exclusion authority");
        assert!(
            format!("{error:#}").contains("untracked ignore control"),
            "{error:#}"
        );

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn footer_generation_allows_build_output_declared_by_tracked_gitignore() {
        let root = source_bound_root_footer_fixture("source-bound-tracked-gitignore");
        fs::create_dir_all(root.join("build-cache")).expect("create declared build cache");
        fs::write(
            root.join("build-cache/generated.o"),
            "generated build output\n",
        )
        .expect("write declared build output");

        assert!(
            super::verification_receipt_commit_footer(&root, "TASK-SOURCE")
                .expect("tracked .gitignore declaration should remain operable")
                .is_some(),
            "ordinary build output excluded by a tracked .gitignore must not invalidate closeout"
        );

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn source_state_byte_bound_accounts_for_normalized_plan_input() {
        let root = temp_dir("source-bound-plan-byte-limit");
        init_git_repo(&root);
        let plan_len = fs::metadata(root.join("IMPLEMENTATION_PLAN.md"))
            .expect("stat plan")
            .len();
        let limits = super::SourceStateLimits {
            max_submodule_depth: super::SOURCE_STATE_MAX_SUBMODULE_DEPTH,
            max_entries: super::SOURCE_STATE_MAX_ENTRIES,
            max_bytes: plan_len.saturating_sub(1),
        };

        let error = super::current_source_state_fingerprint_with_limits(&root, limits)
            .expect_err("normalized plan input must consume the source-state byte bound");
        assert!(
            format!("{error:#}").contains("normalized IMPLEMENTATION_PLAN.md"),
            "{error:#}"
        );

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn source_state_inventory_reader_stops_at_entry_bound_before_draining_input() {
        struct LargeInventory {
            remaining: usize,
        }

        impl std::io::Read for LargeInventory {
            fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
                let read = buffer.len().min(self.remaining);
                for (index, byte) in buffer[..read].iter_mut().enumerate() {
                    *byte = if index % 2 == 0 { b'x' } else { 0 };
                }
                self.remaining -= read;
                Ok(read)
            }
        }

        let mut reader = LargeInventory {
            remaining: 1024 * 1024,
        };
        let mut budget = super::SourceStateBudget::default();
        let limits = super::SourceStateLimits {
            max_submodule_depth: super::SOURCE_STATE_MAX_SUBMODULE_DEPTH,
            max_entries: 1,
            max_bytes: 1024 * 1024,
        };

        let error = super::read_bounded_nul_records(
            &mut reader,
            limits,
            &mut budget,
            "adversarial inventory",
        )
        .expect_err("entry-bound exhaustion must stop an oversized inventory stream");
        assert!(format!("{error:#}").contains("entry bound"), "{error:#}");
        assert!(
            reader.remaining > 0,
            "the collector must stop before draining an oversized producer"
        );
    }

    #[test]
    fn attestation_seam_fails_closed_before_reading_oversized_plan() {
        let root = source_bound_root_footer_fixture("attestation-bounded-plan");
        super::clear_verified_source_attestation(&root, "TASK-SOURCE")
            .expect("clear prior attestation");
        fs::write(root.join("IMPLEMENTATION_PLAN.md"), "x".repeat(4096))
            .expect("write oversized plan");
        let limits = super::SourceStateLimits {
            max_submodule_depth: super::SOURCE_STATE_MAX_SUBMODULE_DEPTH,
            max_entries: super::SOURCE_STATE_MAX_ENTRIES,
            max_bytes: 64,
        };

        let error = super::record_verified_source_attestation_with_source_limits(
            &root,
            "TASK-SOURCE",
            limits,
        )
        .expect_err("attestation must bound its plan input before parsing");
        assert!(
            format!("{error:#}").contains("attestation IMPLEMENTATION_PLAN.md input"),
            "{error:#}"
        );
        assert!(
            !super::verified_source_attestation_path(&root, "TASK-SOURCE").exists(),
            "a bounded-input failure must not publish an attestation"
        );

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn attestation_seam_fails_closed_before_reading_oversized_receipt() {
        let root = source_bound_root_footer_fixture("attestation-bounded-receipt");
        super::clear_verified_source_attestation(&root, "TASK-SOURCE")
            .expect("clear prior attestation");
        let receipt_path = super::verification_receipt_path(&root, "TASK-SOURCE");
        fs::write(&receipt_path, " ".repeat(4096)).expect("write oversized receipt");
        let plan_len = fs::metadata(root.join("IMPLEMENTATION_PLAN.md"))
            .expect("stat plan")
            .len();
        let limits = super::SourceStateLimits {
            max_submodule_depth: super::SOURCE_STATE_MAX_SUBMODULE_DEPTH,
            max_entries: super::SOURCE_STATE_MAX_ENTRIES,
            max_bytes: plan_len + 64,
        };

        let error = super::record_verified_source_attestation_with_source_limits(
            &root,
            "TASK-SOURCE",
            limits,
        )
        .expect_err("attestation must bound its receipt input before parsing");
        assert!(
            format!("{error:#}").contains("attestation verification receipt input"),
            "{error:#}"
        );
        assert!(
            !super::verified_source_attestation_path(&root, "TASK-SOURCE").exists(),
            "a bounded-input failure must not publish an attestation"
        );

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn attestation_seam_bounds_freshness_diff_before_source_collection() {
        let root = source_bound_root_footer_fixture("attestation-bounded-diff");
        super::clear_verified_source_attestation(&root, "TASK-SOURCE")
            .expect("clear prior attestation");
        fs::write(root.join("src/lib.rs"), "changed source\n".repeat(4096))
            .expect("write large unverified source diff");
        write_source_bound_receipt(&root, "TASK-SOURCE");
        let plan_len = fs::metadata(root.join("IMPLEMENTATION_PLAN.md"))
            .expect("stat plan")
            .len();
        let receipt_len = fs::metadata(super::verification_receipt_path(&root, "TASK-SOURCE"))
            .expect("stat receipt")
            .len();
        let limits = super::SourceStateLimits {
            max_submodule_depth: super::SOURCE_STATE_MAX_SUBMODULE_DEPTH,
            max_entries: super::SOURCE_STATE_MAX_ENTRIES,
            max_bytes: plan_len + receipt_len + 256,
        };

        let error = super::record_verified_source_attestation_with_source_limits(
            &root,
            "TASK-SOURCE",
            limits,
        )
        .expect_err("attestation freshness must stream and bound Git diff output");
        assert!(
            format!("{error:#}").contains("unstaged dirty diff"),
            "{error:#}"
        );
        assert!(
            !super::verified_source_attestation_path(&root, "TASK-SOURCE").exists(),
            "a bounded freshness failure must not publish an attestation"
        );

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn footer_seam_fails_closed_before_reading_oversized_receipt() {
        let root = source_bound_root_footer_fixture("footer-bounded-receipt");
        let receipt_len = fs::metadata(super::verification_receipt_path(&root, "TASK-SOURCE"))
            .expect("stat receipt")
            .len();
        let limits = super::SourceStateLimits {
            max_submodule_depth: super::SOURCE_STATE_MAX_SUBMODULE_DEPTH,
            max_entries: super::SOURCE_STATE_MAX_ENTRIES,
            max_bytes: receipt_len.saturating_sub(1),
        };

        let error = super::verification_receipt_commit_footer_with_source_limits(
            &root,
            "TASK-SOURCE",
            limits,
        )
        .expect_err("footer generation must bound its receipt before parsing");
        assert!(
            format!("{error:#}").contains("footer verification receipt input"),
            "{error:#}"
        );

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn footer_seam_charges_plan_after_receipt_under_one_shared_bound() {
        let root = source_bound_root_footer_fixture("footer-bounded-plan");
        let receipt_len = fs::metadata(super::verification_receipt_path(&root, "TASK-SOURCE"))
            .expect("stat receipt")
            .len();
        let plan_len = fs::metadata(root.join("IMPLEMENTATION_PLAN.md"))
            .expect("stat plan")
            .len();
        let limits = super::SourceStateLimits {
            max_submodule_depth: super::SOURCE_STATE_MAX_SUBMODULE_DEPTH,
            max_entries: super::SOURCE_STATE_MAX_ENTRIES,
            max_bytes: receipt_len + plan_len - 1,
        };

        let error = super::verification_receipt_commit_footer_with_source_limits(
            &root,
            "TASK-SOURCE",
            limits,
        )
        .expect_err("footer receipt and plan must share one transaction bound");
        assert!(
            format!("{error:#}").contains("footer IMPLEMENTATION_PLAN.md input"),
            "{error:#}"
        );

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn footer_seam_bounds_verified_source_attestation_input() {
        let root = source_bound_root_footer_fixture("footer-bounded-attestation");
        let receipt_len = fs::metadata(super::verification_receipt_path(&root, "TASK-SOURCE"))
            .expect("stat receipt")
            .len();
        let plan_len = fs::metadata(root.join("IMPLEMENTATION_PLAN.md"))
            .expect("stat plan")
            .len();
        fs::write(
            super::verified_source_attestation_path(&root, "TASK-SOURCE"),
            " ".repeat(4096),
        )
        .expect("write oversized source attestation");
        let limits = super::SourceStateLimits {
            max_submodule_depth: super::SOURCE_STATE_MAX_SUBMODULE_DEPTH,
            max_entries: super::SOURCE_STATE_MAX_ENTRIES,
            max_bytes: receipt_len + plan_len + 64,
        };

        let error = super::verification_receipt_commit_footer_with_source_limits(
            &root,
            "TASK-SOURCE",
            limits,
        )
        .expect_err("footer generation must bound its source attestation before parsing");
        assert!(
            format!("{error:#}").contains("footer verified-source attestation input"),
            "{error:#}"
        );

        let _ = fs::remove_dir_all(root);
    }

    fn source_bound_submodule_footer_fixture(name: &str) -> (PathBuf, PathBuf) {
        let root = temp_dir(name);
        init_git_repo(&root);
        let child_source = temp_dir(&format!("{name}-child-source"));
        init_git_repo(&child_source);
        fs::write(child_source.join("tracked.txt"), "verified child source\n")
            .expect("write child source");
        #[cfg(unix)]
        std::os::unix::fs::symlink("tracked.txt", child_source.join("tracked-link"))
            .expect("write child symlink");
        git_ok(&child_source, ["add", "-A"]);
        git_ok(&child_source, ["commit", "-m", "seed child source"]);

        fs::write(
            root.join("IMPLEMENTATION_PLAN.md"),
            "- [~] `TASK-SUBMODULE` Source-bound submodule proof\n  Owns: `vendor/sub`\n  Verification:\n    - `cargo test source_binding`\n  Completion artifacts: none\n  Dependencies: none\n",
        )
        .expect("write partial plan");
        fs::write(
            root.join("REVIEW.md"),
            "# REVIEW\n\nAwaiting closeout for TASK-SUBMODULE\n",
        )
        .expect("write review");
        let child_source_path = child_source.to_string_lossy().into_owned();
        git_ok(
            &root,
            [
                "-c",
                "protocol.file.allow=always",
                "submodule",
                "add",
                &child_source_path,
                "vendor/sub",
            ],
        );
        git_ok(&root, ["add", "-A"]);
        git_ok(&root, ["commit", "-m", "seed source-bound submodule"]);
        source_bound_footer_fixture(&root, "TASK-SUBMODULE");
        fs::write(
            root.join("IMPLEMENTATION_PLAN.md"),
            "- [x] `TASK-SUBMODULE` Source-bound submodule proof\n  Owns: `vendor/sub`\n  Verification:\n    - `cargo test source_binding`\n  Completion artifacts: none\n  Dependencies: none\n",
        )
        .expect("mark submodule task done");
        fs::write(
            root.join("REVIEW.md"),
            "# REVIEW\n\n## `TASK-SUBMODULE`\n\nIndependent review: CLEAR\n",
        )
        .expect("clear submodule review");
        (root, child_source)
    }

    fn source_bound_nested_submodule_footer_fixture(name: &str) -> (PathBuf, PathBuf, PathBuf) {
        let deep_source = temp_dir(&format!("{name}-deep-source"));
        init_git_repo(&deep_source);
        fs::write(deep_source.join("deep.txt"), "verified deep source\n")
            .expect("write deep source");
        git_ok(&deep_source, ["add", "deep.txt"]);
        git_ok(&deep_source, ["commit", "-m", "seed deep source"]);

        let child_source = temp_dir(&format!("{name}-child-source"));
        init_git_repo(&child_source);
        let deep_source_path = deep_source.to_string_lossy().into_owned();
        git_ok(
            &child_source,
            [
                "-c",
                "protocol.file.allow=always",
                "submodule",
                "add",
                &deep_source_path,
                "vendor/deep",
            ],
        );
        git_ok(&child_source, ["add", "-A"]);
        git_ok(&child_source, ["commit", "-m", "seed nested child"]);

        let root = temp_dir(name);
        init_git_repo(&root);
        fs::write(
            root.join("IMPLEMENTATION_PLAN.md"),
            "- [~] `TASK-SUBMODULE` Source-bound submodule proof\n  Owns: `vendor/sub`\n  Verification:\n    - `cargo test source_binding`\n  Completion artifacts: none\n  Dependencies: none\n",
        )
        .expect("write partial plan");
        fs::write(
            root.join("REVIEW.md"),
            "# REVIEW\n\nAwaiting closeout for TASK-SUBMODULE\n",
        )
        .expect("write review");
        let child_source_path = child_source.to_string_lossy().into_owned();
        git_ok(
            &root,
            [
                "-c",
                "protocol.file.allow=always",
                "submodule",
                "add",
                &child_source_path,
                "vendor/sub",
            ],
        );
        git_ok(
            &root,
            [
                "-c",
                "protocol.file.allow=always",
                "submodule",
                "update",
                "--init",
                "--recursive",
            ],
        );
        git_ok(&root, ["add", "-A"]);
        git_ok(
            &root,
            ["commit", "-m", "seed nested source-bound submodule"],
        );
        source_bound_footer_fixture(&root, "TASK-SUBMODULE");
        fs::write(
            root.join("IMPLEMENTATION_PLAN.md"),
            "- [x] `TASK-SUBMODULE` Source-bound submodule proof\n  Owns: `vendor/sub`\n  Verification:\n    - `cargo test source_binding`\n  Completion artifacts: none\n  Dependencies: none\n",
        )
        .expect("mark nested submodule task done");
        fs::write(
            root.join("REVIEW.md"),
            "# REVIEW\n\n## `TASK-SUBMODULE`\n\nIndependent review: CLEAR\n",
        )
        .expect("clear nested submodule review");
        (root, child_source, deep_source)
    }

    #[test]
    fn footer_generation_rejects_hidden_dirty_submodule_content() {
        let (root, child_source) =
            source_bound_submodule_footer_fixture("source-bound-hidden-submodule");
        git_ok(&root, ["config", "diff.ignoreSubmodules", "all"]);
        git_ok(&root, ["config", "submodule.vendor/sub.ignore", "all"]);
        git_ok(
            &root.join("vendor/sub"),
            ["config", "core.fileMode", "false"],
        );
        fs::write(
            root.join("vendor/sub/tracked.txt"),
            "hidden child mutation\n",
        )
        .expect("mutate checked-out child source");

        let error = super::verification_receipt_commit_footer(&root, "TASK-SUBMODULE")
            .expect_err("hidden submodule dirt must invalidate host source attestation");
        let rendered = format!("{error:#}");
        assert!(rendered.contains("source-state"), "{rendered}");
        assert!(rendered.contains("re-execution"), "{rendered}");

        let _ = fs::remove_dir_all(root);
        let _ = fs::remove_dir_all(child_source);
    }

    #[test]
    fn footer_generation_rejects_hidden_nested_submodule_content() {
        let (root, child_source, deep_source) =
            source_bound_nested_submodule_footer_fixture("source-bound-nested-submodule");
        git_ok(&root, ["config", "diff.ignoreSubmodules", "all"]);
        git_ok(&root, ["config", "submodule.vendor/sub.ignore", "all"]);
        let child = root.join("vendor/sub");
        git_ok(&child, ["config", "diff.ignoreSubmodules", "all"]);
        git_ok(&child, ["config", "submodule.vendor/deep.ignore", "all"]);
        fs::write(
            root.join("vendor/sub/vendor/deep/deep.txt"),
            "hidden deep mutation\n",
        )
        .expect("mutate deep source");

        let error = super::verification_receipt_commit_footer(&root, "TASK-SUBMODULE")
            .expect_err("nested submodule dirt must invalidate attestation");
        assert!(format!("{error:#}").contains("source-state"));

        let _ = fs::remove_dir_all(root);
        let _ = fs::remove_dir_all(child_source);
        let _ = fs::remove_dir_all(deep_source);
    }

    #[test]
    fn footer_generation_accepts_unchanged_recursive_submodule_state() {
        let (root, child_source) =
            source_bound_submodule_footer_fixture("source-bound-stable-submodule");

        let footer = super::verification_receipt_commit_footer(&root, "TASK-SUBMODULE")
            .expect("unchanged submodule state should remain attestable")
            .expect("unchanged submodule state should produce a footer");
        let parsed = super::parse_verification_receipt_footer(
            "deadbeef",
            &format!("fixture closeout\n\n{footer}"),
        )
        .expect("parse generated footer");
        let embedded: serde_json::Value =
            serde_json::from_str(&parsed.receipt_text).expect("parse embedded receipt");
        assert!(embedded.get("source_state_v2").is_some());
        assert!(embedded.get("source_state_v1").is_none());

        let _ = fs::remove_dir_all(root);
        let _ = fs::remove_dir_all(child_source);
    }

    #[test]
    fn footer_generation_rejects_checked_out_submodule_head_change() {
        let (root, child_source) =
            source_bound_submodule_footer_fixture("source-bound-submodule-head");
        let checkout = root.join("vendor/sub");
        git_ok(&checkout, ["config", "user.email", "test@example.com"]);
        git_ok(&checkout, ["config", "user.name", "Test User"]);
        fs::write(checkout.join("tracked.txt"), "new committed child source\n")
            .expect("change child source");
        git_ok(&checkout, ["add", "tracked.txt"]);
        git_ok(&checkout, ["commit", "-m", "advance checked-out child"]);

        let error = super::verification_receipt_commit_footer(&root, "TASK-SUBMODULE")
            .expect_err("a changed checked-out submodule HEAD must invalidate attestation");
        assert!(format!("{error:#}").contains("source-state"));

        let _ = fs::remove_dir_all(root);
        let _ = fs::remove_dir_all(child_source);
    }

    #[test]
    fn footer_generation_rejects_gitlink_index_identity_change() {
        let (root, child_source) =
            source_bound_submodule_footer_fixture("source-bound-gitlink-index");
        fs::write(
            child_source.join("tracked.txt"),
            "different indexed child commit\n",
        )
        .expect("advance child source");
        git_ok(&child_source, ["add", "tracked.txt"]);
        git_ok(&child_source, ["commit", "-m", "advance gitlink target"]);
        let new_gitlink = git_stdout(&child_source, ["rev-parse", "HEAD"]);
        git_ok(&root, ["config", "diff.ignoreSubmodules", "all"]);
        git_ok(&root, ["config", "submodule.vendor/sub.ignore", "all"]);
        git_ok(
            &root,
            [
                "update-index",
                "--cacheinfo",
                "160000",
                &new_gitlink,
                "vendor/sub",
            ],
        );

        let error = super::verification_receipt_commit_footer(&root, "TASK-SUBMODULE")
            .expect_err("a changed gitlink index object must invalidate attestation");
        assert!(format!("{error:#}").contains("source-state"));

        let _ = fs::remove_dir_all(root);
        let _ = fs::remove_dir_all(child_source);
    }

    #[test]
    fn footer_generation_rejects_version_one_source_attestation() {
        let (root, child_source) =
            source_bound_submodule_footer_fixture("source-bound-v1-attestation");
        let attestation_path = root
            .join(".auto/parallel/verified-source")
            .join("TASK-SUBMODULE.json");
        let mut attestation: serde_json::Value =
            serde_json::from_slice(&fs::read(&attestation_path).expect("read current attestation"))
                .expect("parse current attestation");
        let object = attestation
            .as_object_mut()
            .expect("attestation should be an object");
        object.insert("version".to_string(), serde_json::json!(1));
        let source_state = object
            .remove("source_state_v2")
            .expect("version 2 attestation should carry source_state_v2");
        object.insert("source_state_v1".to_string(), source_state);
        fs::write(
            &attestation_path,
            serde_json::to_vec_pretty(&attestation).expect("render legacy attestation"),
        )
        .expect("replace attestation with version 1 shape");

        let error = super::verification_receipt_commit_footer(&root, "TASK-SUBMODULE")
            .expect_err("version 1 source attestations must be historical-only");
        assert!(
            format!("{error:#}").contains("invalid host verified-source attestation"),
            "{error:#}"
        );

        let _ = fs::remove_dir_all(root);
        let _ = fs::remove_dir_all(child_source);
    }

    #[test]
    fn footer_generation_fails_closed_on_source_state_entry_bound() {
        let (root, child_source) =
            source_bound_submodule_footer_fixture("source-bound-entry-limit");
        let limits = super::SourceStateLimits {
            max_submodule_depth: super::SOURCE_STATE_MAX_SUBMODULE_DEPTH,
            max_entries: 0,
            max_bytes: super::SOURCE_STATE_MAX_BYTES,
        };

        let error = super::verification_receipt_commit_footer_with_source_limits(
            &root,
            "TASK-SUBMODULE",
            limits,
        )
        .expect_err("entry-bound exhaustion must fail closeout closed");
        assert!(format!("{error:#}").contains("entry bound"), "{error:#}");

        let _ = fs::remove_dir_all(root);
        let _ = fs::remove_dir_all(child_source);
    }

    #[test]
    fn footer_generation_fails_closed_on_source_state_byte_bound() {
        let (root, child_source) = source_bound_submodule_footer_fixture("source-bound-byte-limit");
        let limits = super::SourceStateLimits {
            max_submodule_depth: super::SOURCE_STATE_MAX_SUBMODULE_DEPTH,
            max_entries: super::SOURCE_STATE_MAX_ENTRIES,
            max_bytes: 1,
        };

        let error = super::verification_receipt_commit_footer_with_source_limits(
            &root,
            "TASK-SUBMODULE",
            limits,
        )
        .expect_err("byte-bound exhaustion must fail closeout closed");
        assert!(format!("{error:#}").contains("byte bound"), "{error:#}");

        let _ = fs::remove_dir_all(root);
        let _ = fs::remove_dir_all(child_source);
    }

    #[test]
    fn footer_generation_fails_closed_on_source_state_depth_bound() {
        let (root, child_source, deep_source) =
            source_bound_nested_submodule_footer_fixture("source-bound-depth-limit");
        let limits = super::SourceStateLimits {
            max_submodule_depth: 0,
            max_entries: super::SOURCE_STATE_MAX_ENTRIES,
            max_bytes: super::SOURCE_STATE_MAX_BYTES,
        };

        let error = super::verification_receipt_commit_footer_with_source_limits(
            &root,
            "TASK-SUBMODULE",
            limits,
        )
        .expect_err("depth-bound exhaustion must fail closeout closed");
        assert!(format!("{error:#}").contains("depth bound"), "{error:#}");

        let _ = fs::remove_dir_all(root);
        let _ = fs::remove_dir_all(child_source);
        let _ = fs::remove_dir_all(deep_source);
    }

    #[test]
    fn source_state_cycle_detection_fails_closed() {
        let root = temp_dir("source-bound-cycle");
        init_git_repo(&root);
        let canonical = fs::canonicalize(&root).expect("canonicalize source fixture");
        let mut budget = super::SourceStateBudget::default();
        budget.visited_repositories.insert(canonical);
        let mut hasher = sha2::Sha256::new();

        let error = super::collect_repository_source_state(
            &root,
            "",
            0,
            super::SourceStateLimits::default(),
            &mut budget,
            &mut hasher,
        )
        .expect_err("revisiting a canonical repository must fail closed");
        assert!(format!("{error:#}").contains("cycle"));

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn footer_generation_rejects_untracked_submodule_content() {
        let (root, child_source) =
            source_bound_submodule_footer_fixture("source-bound-submodule-untracked");
        fs::write(
            root.join("vendor/sub/untracked.txt"),
            "unverified child source\n",
        )
        .expect("write untracked child source");

        let error = super::verification_receipt_commit_footer(&root, "TASK-SUBMODULE")
            .expect_err("untracked child source must invalidate attestation");
        assert!(format!("{error:#}").contains("source-state"));

        let _ = fs::remove_dir_all(root);
        let _ = fs::remove_dir_all(child_source);
    }

    #[cfg(unix)]
    #[test]
    fn footer_generation_rejects_submodule_mode_change_hidden_by_core_filemode() {
        use std::os::unix::fs::PermissionsExt;

        let (root, child_source) =
            source_bound_submodule_footer_fixture("source-bound-submodule-mode");
        let checkout = root.join("vendor/sub");
        git_ok(&root, ["config", "diff.ignoreSubmodules", "all"]);
        git_ok(&root, ["config", "submodule.vendor/sub.ignore", "all"]);
        git_ok(&checkout, ["config", "core.fileMode", "false"]);
        let tracked = checkout.join("tracked.txt");
        let mut permissions = fs::metadata(&tracked)
            .expect("stat tracked child source")
            .permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&tracked, permissions).expect("change child executable mode");

        let error = super::verification_receipt_commit_footer(&root, "TASK-SUBMODULE")
            .expect_err("actual child mode drift must invalidate attestation");
        assert!(format!("{error:#}").contains("source-state"));

        let _ = fs::remove_dir_all(root);
        let _ = fs::remove_dir_all(child_source);
    }

    #[cfg(unix)]
    #[test]
    fn footer_generation_rejects_changed_submodule_symlink_target() {
        let (root, child_source) =
            source_bound_submodule_footer_fixture("source-bound-submodule-symlink");
        let link = root.join("vendor/sub/tracked-link");
        fs::remove_file(&link).expect("remove verified child symlink");
        std::os::unix::fs::symlink("different-target", &link)
            .expect("replace child symlink target");

        let error = super::verification_receipt_commit_footer(&root, "TASK-SUBMODULE")
            .expect_err("child symlink target drift must invalidate attestation");
        assert!(format!("{error:#}").contains("source-state"));

        let _ = fs::remove_dir_all(root);
        let _ = fs::remove_dir_all(child_source);
    }

    #[test]
    fn footer_generation_accepts_queue_only_changes_after_source_bound_verification() {
        let root = temp_dir("source-bound-footer-queue-only");
        init_git_repo(&root);
        fs::create_dir_all(root.join("src")).expect("failed to create source directory");
        fs::write(root.join("src/lib.rs"), "pub fn verified() {}\n")
            .expect("failed to write source");
        fs::write(root.join(".gitignore"), ".auto/\n").expect("failed to write gitignore");
        fs::write(
            root.join("IMPLEMENTATION_PLAN.md"),
            "- [~] `TASK-SOURCE` Source-bound proof\n  Owns: `src/lib.rs`\n  Verification:\n    - `cargo test source_binding`\n  Completion artifacts: none\n  Dependencies: none\n",
        )
        .expect("failed to write partial plan");
        fs::write(
            root.join("REVIEW.md"),
            "# REVIEW\n\nAwaiting closeout for TASK-SOURCE\n",
        )
        .expect("failed to write review");
        Command::new("git")
            .arg("-C")
            .arg(&root)
            .args([
                "add",
                "src/lib.rs",
                ".gitignore",
                "IMPLEMENTATION_PLAN.md",
                "REVIEW.md",
            ])
            .output()
            .expect("git add source fixture failed");
        Command::new("git")
            .arg("-C")
            .arg(&root)
            .args(["commit", "-m", "task source"])
            .output()
            .expect("git commit source fixture failed");

        source_bound_footer_fixture(&root, "TASK-SOURCE");

        fs::write(
            root.join("IMPLEMENTATION_PLAN.md"),
            "- [x] `TASK-SOURCE` Source-bound proof\n  Owns: `src/lib.rs`\n  Verification:\n    - `cargo test source_binding`\n  Completion artifacts: none\n  Dependencies: none\n",
        )
        .expect("failed to mark task done");
        fs::write(
            root.join("REVIEW.md"),
            "# REVIEW\n\n## `TASK-SOURCE`\n\nIndependent review: CLEAR\n",
        )
        .expect("failed to update review handoff");

        assert!(
            super::verification_receipt_commit_footer(&root, "TASK-SOURCE")
                .expect("queue-only closeout should stay source-bound")
                .is_some(),
            "host queue mutations must not invalidate unchanged verified source"
        );
    }

    #[test]
    fn footer_generation_rejects_worktree_or_index_source_changes_after_verification() {
        let root = temp_dir("source-bound-footer-source-change");
        init_git_repo(&root);
        fs::create_dir_all(root.join("src")).expect("failed to create source directory");
        fs::write(root.join("src/lib.rs"), "pub fn verified() -> u8 { 1 }\n")
            .expect("failed to write verified source");
        fs::write(root.join(".gitignore"), ".auto/\n").expect("failed to write gitignore");
        fs::write(
            root.join("IMPLEMENTATION_PLAN.md"),
            "- [~] `TASK-SOURCE` Source-bound proof\n  Owns: `src/lib.rs`\n  Verification:\n    - `cargo test source_binding`\n  Completion artifacts: none\n  Dependencies: none\n",
        )
        .expect("failed to write partial plan");
        Command::new("git")
            .arg("-C")
            .arg(&root)
            .args(["add", "src/lib.rs", ".gitignore", "IMPLEMENTATION_PLAN.md"])
            .output()
            .expect("git add source fixture failed");
        Command::new("git")
            .arg("-C")
            .arg(&root)
            .args(["commit", "-m", "verified source"])
            .output()
            .expect("git commit source fixture failed");

        source_bound_footer_fixture(&root, "TASK-SOURCE");

        fs::write(
            root.join("IMPLEMENTATION_PLAN.md"),
            "- [x] `TASK-SOURCE` Source-bound proof\n  Owns: `src/lib.rs`\n  Verification:\n    - `cargo test source_binding`\n  Completion artifacts: none\n  Dependencies: none\n",
        )
        .expect("failed to mark task done");
        fs::write(root.join("src/lib.rs"), "pub fn verified() -> u8 { 2 }\n")
            .expect("failed to change verified source");

        let error = super::verification_receipt_commit_footer(&root, "TASK-SOURCE")
            .expect_err("changed source must not be footerized from stale JSON");
        let rendered = format!("{error:#}");
        assert!(rendered.contains("source-state"), "{rendered}");
        assert!(rendered.contains("re-execution"), "{rendered}");

        // A staged blob can differ from both HEAD and the worktree. Source
        // attestation must bind that index state too: a bare closeout commit
        // consumes the index, not merely the worktree bytes.
        fs::write(root.join("src/lib.rs"), "pub fn verified() -> u8 { 9 }\n")
            .expect("failed to write staged source injection");
        Command::new("git")
            .arg("-C")
            .arg(&root)
            .args(["add", "src/lib.rs"])
            .output()
            .expect("failed to stage source injection");
        fs::write(root.join("src/lib.rs"), "pub fn verified() -> u8 { 1 }\n")
            .expect("failed to restore verified worktree source");

        let error = super::verification_receipt_commit_footer(&root, "TASK-SOURCE")
            .expect_err("staged source differing from the worktree must invalidate attestation");
        let rendered = format!("{error:#}");
        assert!(rendered.contains("source-state"), "{rendered}");
        assert!(rendered.contains("re-execution"), "{rendered}");
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
        let receipt_path = root.join(".auto/symphony/verification-receipts/TASK-METADATA.json");
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
            source_state_v1: None,
            source_state_v2: None,
            declared_artifacts: vec![VerificationReceiptArtifact {
                path: "docs/ops/proof.md".to_string(),
                sha256: Some(artifact_hash.clone()),
            }],
            task_owned_inputs_v1: None,
            commands: vec![VerificationReceiptCommand {
                command: expected_command.clone(),
                argv: expected_argv.clone(),
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
    fn verification_receipt_freshness_accepts_task_wrapper_inner_expected_argv() {
        let root = temp_dir("task-wrapper-inner-expected-argv");
        init_git_repo(&root);
        fs::create_dir_all(root.join(".auto/symphony/verification-receipts"))
            .expect("failed to create receipts dir");
        let receipt_path = root.join(".auto/symphony/verification-receipts/TASK-WRAPPED.json");
        fs::write(&receipt_path, "{}\n").expect("failed to write receipt placeholder");

        let inner = "cargo test -p demo --all-features --locked";
        let expected_command =
            format!("scripts/run-task-verification.sh TASK-WRAPPED -- bash -lc '{inner}'");
        let receipt = VerificationReceipt {
            task_id: Some("TASK-WRAPPED".to_string()),
            commit: super::current_git_commit(&root),
            dirty_state: Some(VerificationDirtyState {
                fingerprint: super::current_dirty_state_fingerprint(&root),
                ..VerificationDirtyState::default()
            }),
            plan_hash: super::current_plan_hash(&root),
            commands: vec![VerificationReceiptCommand {
                command: format!("bash -lc '{inner}'"),
                argv: vec!["bash".to_string(), "-lc".to_string(), inner.to_string()],
                expected_argv: Some(vec![
                    "bash".to_string(),
                    "-lc".to_string(),
                    inner.to_string(),
                ]),
                exit_code: Some(0),
                status: Some("passed".to_string()),
                ..VerificationReceiptCommand::default()
            }],
            ..VerificationReceipt::default()
        };

        assert_eq!(
            verification_receipt_freshness_problem(
                &root,
                &receipt_path,
                &receipt,
                &[expected_command],
                &[],
            ),
            None
        );

        let wrong_task =
            format!("scripts/run-task-verification.sh TASK-OTHER -- bash -lc '{inner}'");
        let problem = verification_receipt_freshness_problem(
            &root,
            &receipt_path,
            &receipt,
            &[wrong_task],
            &[],
        )
        .expect("wrapper task id must bind to the receipt task");
        assert!(problem.contains("TASK-OTHER"), "{problem}");
        assert!(problem.contains("TASK-WRAPPED"), "{problem}");

        let malformed = format!(
            "scripts/run-task-verification.sh TASK-WRAPPED --label forged -- bash -lc '{inner}'"
        );
        let problem = verification_receipt_freshness_problem(
            &root,
            &receipt_path,
            &receipt,
            &[malformed],
            &[],
        )
        .expect("wrapper must have exactly TASK -- INNER shape");
        assert!(problem.contains("malformed"), "{problem}");
    }

    #[test]
    fn verification_receipt_identity_rejects_cross_task_replay_for_plain_commands_and_footers() {
        let root = temp_dir("cross-task-receipt-identity");
        let receipt_path = root.join("TASK-EXPECTED.json");
        let receipt = VerificationReceipt {
            task_id: Some("TASK-OTHER".to_string()),
            commands: vec![VerificationReceiptCommand {
                command: "cargo test -p demo task_expected".to_string(),
                argv: vec![
                    "cargo".to_string(),
                    "test".to_string(),
                    "-p".to_string(),
                    "demo".to_string(),
                    "task_expected".to_string(),
                ],
                exit_code: Some(0),
                status: Some("passed".to_string()),
                ..VerificationReceiptCommand::default()
            }],
            ..VerificationReceipt::default()
        };

        let problem = verification_receipt_freshness_problem(
            &root,
            &receipt_path,
            &receipt,
            &["cargo test -p demo task_expected".to_string()],
            &[],
        )
        .expect("a plain-command JSON receipt must bind to its task path");
        assert!(problem.contains("TASK-OTHER"), "{problem}");
        assert!(problem.contains("TASK-EXPECTED"), "{problem}");

        let footer = super::VerificationReceiptFooter {
            task_id: "TASK-EXPECTED".to_string(),
            commit: "0000000000000000000000000000000000000000".to_string(),
            receipt_text: serde_json::to_string(&receipt).expect("serialize receipt"),
        };
        let problem = super::shared_footer_receipt_freshness_problem(
            &root,
            &footer,
            &["cargo test -p demo task_expected".to_string()],
            &[],
        )
        .expect("footer inspection should be bounded")
        .expect("footer receipt task_id must bind to the footer task");
        assert!(problem.contains("TASK-OTHER"), "{problem}");
        assert!(problem.contains("TASK-EXPECTED"), "{problem}");
    }

    #[test]
    fn verification_receipt_identity_requires_embedded_task_for_plain_commands_and_footers() {
        let root = temp_dir("missing-receipt-identity");
        let receipt_path = root.join("TASK-EXPECTED.json");
        let expected = "cargo test -p demo task_expected".to_string();
        let receipt = VerificationReceipt {
            task_id: None,
            commands: vec![VerificationReceiptCommand {
                command: expected.clone(),
                argv: vec![
                    "cargo".to_string(),
                    "test".to_string(),
                    "-p".to_string(),
                    "demo".to_string(),
                    "task_expected".to_string(),
                ],
                expected_argv: Some(vec![
                    "cargo".to_string(),
                    "test".to_string(),
                    "-p".to_string(),
                    "demo".to_string(),
                    "task_expected".to_string(),
                ]),
                exit_code: Some(0),
                status: Some("passed".to_string()),
                ..VerificationReceiptCommand::default()
            }],
            ..VerificationReceipt::default()
        };

        let problem = verification_receipt_freshness_problem(
            &root,
            &receipt_path,
            &receipt,
            std::slice::from_ref(&expected),
            &[],
        )
        .expect("plain-command JSON receipts must carry their task identity");
        assert!(problem.contains("missing task_id"), "{problem}");

        let footer = super::VerificationReceiptFooter {
            task_id: "TASK-EXPECTED".to_string(),
            commit: "0000000000000000000000000000000000000000".to_string(),
            receipt_text: serde_json::to_string(&receipt).expect("serialize receipt"),
        };
        let problem = super::shared_footer_receipt_freshness_problem(
            &root,
            &footer,
            std::slice::from_ref(&expected),
            &[],
        )
        .expect("footer inspection should be bounded")
        .expect("footer receipts must carry the same embedded task identity");
        assert!(problem.contains("missing task_id"), "{problem}");
    }

    #[test]
    fn verification_receipt_exact_command_text_never_overrides_actual_argv() {
        let root = temp_dir("exact-command-wrong-argv");
        init_git_repo(&root);
        let receipt_path = root.join(".auto/symphony/verification-receipts/TASK-ARGV.json");
        let expected = "cargo test -p demo task_expected".to_string();
        let receipt = VerificationReceipt {
            task_id: Some("TASK-ARGV".to_string()),
            commit: super::current_git_commit(&root),
            dirty_state: Some(VerificationDirtyState {
                fingerprint: super::current_dirty_state_fingerprint(&root),
                ..VerificationDirtyState::default()
            }),
            plan_hash: super::current_plan_hash(&root),
            commands: vec![VerificationReceiptCommand {
                command: expected.clone(),
                argv: vec!["true".to_string()],
                expected_argv: Some(vec![
                    "cargo".to_string(),
                    "test".to_string(),
                    "-p".to_string(),
                    "demo".to_string(),
                    "task_expected".to_string(),
                ]),
                exit_code: Some(0),
                status: Some("passed".to_string()),
                ..VerificationReceiptCommand::default()
            }],
            ..VerificationReceipt::default()
        };

        let problem = verification_receipt_freshness_problem(
            &root,
            &receipt_path,
            &receipt,
            std::slice::from_ref(&expected),
            &[],
        )
        .expect("claimed command text cannot replace proof of the executed argv");
        assert!(problem.contains("actual argv"), "{problem}");

        let footer = super::VerificationReceiptFooter {
            task_id: "TASK-ARGV".to_string(),
            commit: super::current_git_commit(&root).expect("git commit"),
            receipt_text: serde_json::to_string(&receipt).expect("serialize receipt"),
        };
        let problem = super::shared_footer_receipt_freshness_problem(
            &root,
            &footer,
            std::slice::from_ref(&expected),
            &[],
        )
        .expect("footer inspection should be bounded")
        .expect("durable footer command proof must bind to actual argv");
        assert!(problem.contains("actual argv"), "{problem}");

        let mut missing_actual = receipt;
        missing_actual.commands[0].argv.clear();
        let problem = verification_receipt_freshness_problem(
            &root,
            &receipt_path,
            &missing_actual,
            std::slice::from_ref(&expected),
            &[],
        )
        .expect("self-asserted command and expected argv cannot replace missing actual argv");
        assert!(problem.contains("actual argv"), "{problem}");
    }

    #[test]
    fn verification_receipt_footer_parser_rejects_duplicate_reserved_fields() {
        let encoded = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .encode(br#"{"task_id":"TASK-DUPLICATE","commands":[]}"#);
        let body = format!(
            "repo: TASK-DUPLICATE queue sync\n\n\
             Auto-Verification-Receipt-Version: 1\n\
             Auto-Verification-Receipt-Task: TASK-DUPLICATE\n\
             Auto-Verification-Receipt-Task: TASK-OTHER\n\
             Auto-Verification-Receipt-JSON: {encoded}"
        );

        assert!(
            super::parse_verification_receipt_footer("deadbeef", &body).is_none(),
            "duplicate reserved footer fields must not use last-value-wins parsing"
        );
    }

    #[test]
    fn verification_receipt_footer_discovery_requires_host_closeout_provenance() {
        let root = temp_dir("footer-host-provenance");
        init_git_repo(&root);
        fs::write(
            root.join("IMPLEMENTATION_PLAN.md"),
            "- [~] `TASK-HOST` Durable proof\n  Verification:\n    - `cargo test host_proof`\n  Completion artifacts: none\n  Dependencies: none\n",
        )
        .expect("write plan");
        fs::write(
            root.join("REVIEW.md"),
            "# REVIEW\n\nAwaiting auto review:\n## `TASK-HOST`\n",
        )
        .expect("write review");
        Command::new("git")
            .arg("-C")
            .arg(&root)
            .args(["add", "IMPLEMENTATION_PLAN.md", "REVIEW.md"])
            .output()
            .expect("git add partial queue state");
        Command::new("git")
            .arg("-C")
            .arg(&root)
            .args(["commit", "-m", "task host partial"])
            .output()
            .expect("git commit partial queue state");
        fs::create_dir_all(root.join(".auto/symphony/verification-receipts"))
            .expect("create receipts");
        let commit = super::current_git_commit(&root).expect("current commit");
        let dirty_state = super::current_dirty_state_fingerprint(&root).expect("dirty fingerprint");
        let plan_hash = super::current_plan_hash(&root).expect("plan hash");
        fs::write(
            root.join(".auto/symphony/verification-receipts/TASK-HOST.json"),
            format!(
                r#"{{"task_id":"TASK-HOST","commit":"{commit}","dirty_state":{{"fingerprint":"{dirty_state}"}},"plan_hash":"{plan_hash}","commands":[{{"command":"cargo test host_proof","argv":["cargo","test","host_proof"],"expected_argv":["cargo","test","host_proof"],"exit_code":0,"status":"passed"}}]}}"#
            ),
        )
        .expect("write receipt");
        super::record_verified_source_attestation(&root, "TASK-HOST")
            .expect("host verification should attest source");
        fs::write(
            root.join("IMPLEMENTATION_PLAN.md"),
            "- [x] `TASK-HOST` Durable proof\n  Verification:\n    - `cargo test host_proof`\n  Completion artifacts: none\n  Dependencies: none\n",
        )
        .expect("mark task done");
        fs::write(
            root.join("REVIEW.md"),
            "# REVIEW\n\n## `TASK-HOST`\n\nIndependent review: CLEAR\n",
        )
        .expect("clear review");
        let footer = super::verification_receipt_commit_footer(&root, "TASK-HOST")
            .expect("footer preparation")
            .expect("footer");
        Command::new("git")
            .arg("-C")
            .arg(&root)
            .args(["add", "IMPLEMENTATION_PLAN.md", "REVIEW.md"])
            .output()
            .expect("git add plan");
        Command::new("git")
            .arg("-C")
            .arg(&root)
            .args(["commit", "-m", "repo: TASK-HOST queue sync", "-m", &footer])
            .output()
            .expect("git commit host closeout");
        let trusted_head = super::current_git_commit(&root).expect("trusted head");

        let trusted = super::git_verification_receipt_footers(&root);
        assert_eq!(trusted.len(), 1, "host closeout footer should be durable");
        assert_eq!(trusted[0].commit, trusted_head);

        fs::write(root.join("lane-owned.rs"), "pub fn forged() {}\n").expect("write lane source");
        Command::new("git")
            .arg("-C")
            .arg(&root)
            .args(["add", "lane-owned.rs"])
            .output()
            .expect("git add source");
        Command::new("git")
            .arg("-C")
            .arg(&root)
            .args(["commit", "-m", "repo: TASK-HOST queue sync", "-m", &footer])
            .output()
            .expect("git commit forged footer");

        let after_forgery = super::git_verification_receipt_footers(&root);
        assert_eq!(
            after_forgery.len(),
            1,
            "a source-changing commit must not mint host receipt provenance"
        );
        assert_eq!(after_forgery[0].commit, trusted_head);
    }

    #[test]
    fn verification_receipt_footer_discovery_rejects_cross_task_done_transition() {
        let root = temp_dir("footer-cross-task-transition");
        init_git_repo(&root);
        fs::write(
            root.join("IMPLEMENTATION_PLAN.md"),
            "- [~] `TASK-A` First task\n  Verification:\n    - `cargo test task_a`\n  Completion artifacts: none\n  Dependencies: none\n\n- [~] `TASK-B` Second task\n  Verification:\n    - `cargo test source_binding`\n  Completion artifacts: none\n  Dependencies: none\n",
        )
        .expect("write partial plan");
        fs::write(
            root.join("REVIEW.md"),
            "# REVIEW\n\nAwaiting auto review:\n## `TASK-B`\n",
        )
        .expect("write review");
        Command::new("git")
            .arg("-C")
            .arg(&root)
            .args(["add", "IMPLEMENTATION_PLAN.md", "REVIEW.md"])
            .output()
            .expect("git add partial plan");
        Command::new("git")
            .arg("-C")
            .arg(&root)
            .args(["commit", "-m", "two tasks partial"])
            .output()
            .expect("git commit partial plan");

        source_bound_footer_fixture(&root, "TASK-B");
        fs::write(
            root.join("IMPLEMENTATION_PLAN.md"),
            "- [~] `TASK-A` First task\n  Verification:\n    - `cargo test task_a`\n  Completion artifacts: none\n  Dependencies: none\n\n- [x] `TASK-B` Second task\n  Verification:\n    - `cargo test source_binding`\n  Completion artifacts: none\n  Dependencies: none\n",
        )
        .expect("mark only footer task done");
        fs::write(
            root.join("REVIEW.md"),
            "# REVIEW\n\n## `TASK-B`\n\nIndependent review: CLEAR\n",
        )
        .expect("clear task B review");
        let footer = super::verification_receipt_commit_footer(&root, "TASK-B")
            .expect("prepare task B footer")
            .expect("task B footer");

        fs::write(
            root.join("IMPLEMENTATION_PLAN.md"),
            "- [x] `TASK-A` First task\n  Verification:\n    - `cargo test task_a`\n  Completion artifacts: none\n  Dependencies: none\n\n- [x] `TASK-B` Second task\n  Verification:\n    - `cargo test source_binding`\n  Completion artifacts: none\n  Dependencies: none\n",
        )
        .expect("inject cross-task completion transition");
        Command::new("git")
            .arg("-C")
            .arg(&root)
            .args(["add", "IMPLEMENTATION_PLAN.md", "REVIEW.md"])
            .output()
            .expect("git add forged closeout");
        Command::new("git")
            .arg("-C")
            .arg(&root)
            .args(["commit", "-m", "repo: TASK-B queue sync", "-m", &footer])
            .output()
            .expect("git commit forged cross-task closeout");

        assert!(
            super::git_verification_receipt_footers(&root).is_empty(),
            "a footer may only attest the exact parent-to-commit transition for its own task"
        );
    }

    #[test]
    fn verification_receipt_footer_generation_rejects_cross_task_identity() {
        let root = temp_dir("footer-generation-identity");
        init_git_repo(&root);
        fs::write(
            root.join("IMPLEMENTATION_PLAN.md"),
            "- [x] `TASK-EXPECTED` Durable proof\n  Verification:\n    - `cargo test expected`\n  Completion artifacts: none\n  Dependencies: none\n",
        )
        .expect("write plan");
        fs::create_dir_all(root.join(".auto/symphony/verification-receipts"))
            .expect("create receipts");
        fs::write(
            root.join(".auto/symphony/verification-receipts/TASK-EXPECTED.json"),
            r#"{"task_id":"TASK-OTHER","commands":[{"command":"cargo test expected","argv":["cargo","test","expected"],"expected_argv":["cargo","test","expected"],"exit_code":0,"status":"passed"}]}"#,
        )
        .expect("write receipt");

        let error = super::verification_receipt_commit_footer(&root, "TASK-EXPECTED")
            .expect_err("cross-task receipt must not become a durable footer");
        assert!(format!("{error:#}").contains("TASK-OTHER"));
        assert!(format!("{error:#}").contains("TASK-EXPECTED"));
    }

    #[test]
    fn verification_receipt_rejects_ancestor_json_as_current_staging_proof() {
        let root = temp_dir("ancestor-json-receipt");
        init_git_repo(&root);
        let ancestor = super::current_git_commit(&root).expect("initial commit");
        fs::write(root.join("advance.txt"), "advance\n").expect("write advance");
        Command::new("git")
            .arg("-C")
            .arg(&root)
            .args(["add", "advance.txt"])
            .output()
            .expect("git add");
        Command::new("git")
            .arg("-C")
            .arg(&root)
            .args(["commit", "-m", "advance"])
            .output()
            .expect("git commit");

        let expected = "cargo test -p demo ancestor".to_string();
        let receipt = VerificationReceipt {
            task_id: Some("TASK-ANCESTOR".to_string()),
            commit: Some(ancestor),
            commands: vec![VerificationReceiptCommand {
                command: expected.clone(),
                argv: vec![
                    "cargo".to_string(),
                    "test".to_string(),
                    "-p".to_string(),
                    "demo".to_string(),
                    "ancestor".to_string(),
                ],
                expected_argv: Some(vec![
                    "cargo".to_string(),
                    "test".to_string(),
                    "-p".to_string(),
                    "demo".to_string(),
                    "ancestor".to_string(),
                ]),
                exit_code: Some(0),
                status: Some("passed".to_string()),
                ..VerificationReceiptCommand::default()
            }],
            ..VerificationReceipt::default()
        };
        let receipt_path = root.join("TASK-ANCESTOR.json");
        let problem = verification_receipt_freshness_problem(
            &root,
            &receipt_path,
            &receipt,
            &[expected],
            &[],
        )
        .expect("JSON staging evidence must be current, not merely ancestral");
        assert!(problem.contains("not current HEAD"), "{problem}");
    }

    #[test]
    fn command_matching_never_unwraps_an_untrusted_launcher_basename() {
        let expected = "cargo test -p demo".to_string();
        let entry = VerificationReceiptCommand {
            command: "/tmp/bash -lc 'cargo test -p demo'".to_string(),
            argv: vec!["/tmp/bash".to_string(), "-lc".to_string(), expected.clone()],
            expected_argv: Some(vec![
                "/tmp/bash".to_string(),
                "-lc".to_string(),
                expected.clone(),
            ]),
            ..VerificationReceiptCommand::default()
        };
        assert!(
            !super::verification_receipt_command_matches(&entry, &expected),
            "an arbitrary executable named bash must not stand in for the declared command"
        );
    }

    #[test]
    fn verification_receipt_freshness_accepts_legacy_clean_dirty_entries() {
        let root = temp_dir("legacy-clean-dirty-entries");
        init_git_repo(&root);
        let receipt_path = root.join(".auto/symphony/verification-receipts/TASK-LEGACY.json");
        let expected_command = "npm run typecheck".to_string();
        let receipt = VerificationReceipt {
            task_id: Some("TASK-LEGACY".to_string()),
            commit: super::current_git_commit(&root),
            dirty_state: Some(VerificationDirtyState {
                fingerprint: None,
                entries: Vec::new(),
            }),
            plan_hash: super::current_plan_hash(&root),
            source_state_v1: None,
            source_state_v2: None,
            declared_artifacts: Vec::new(),
            task_owned_inputs_v1: None,
            commands: vec![VerificationReceiptCommand {
                command: expected_command.clone(),
                argv: vec![
                    "npm".to_string(),
                    "run".to_string(),
                    "typecheck".to_string(),
                ],
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
        let receipt_path = root.join(".auto/symphony/verification-receipts/TASK-MUTABLE.json");
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
            source_state_v1: None,
            source_state_v2: None,
            declared_artifacts: vec![VerificationReceiptArtifact {
                path: "REVIEW.md".to_string(),
                sha256: Some("not-the-current-review-hash".to_string()),
            }],
            task_owned_inputs_v1: None,
            commands: vec![VerificationReceiptCommand {
                command: expected_command.clone(),
                argv: expected_argv.clone(),
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
            "source_state_v2",
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
            r#"{{"task_id":"TASK","commit":"{}","commands":[{{"command":"cargo test completion_artifacts::tests::shared_receipt","argv":["cargo","test","completion_artifacts::tests::shared_receipt"],"expected_argv":["cargo","test","completion_artifacts::tests::shared_receipt"],"exit_code":0,"status":"passed"}}]}}"#,
            "1111111111111111111111111111111111111111"
        );
        let problem = super::direct_verification_receipt_freshness_problem(
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

    #[test]
    fn direct_freshness_reports_git_head_collection_failure() {
        let root = temp_dir("direct-freshness-missing-git");
        fs::write(root.join("IMPLEMENTATION_PLAN.md"), "# plan\n").expect("write plan");
        let receipt_path = root.join(".auto/symphony/verification-receipts/TASK-GIT-FAIL.json");
        let receipt_text = r#"{"task_id":"TASK-GIT-FAIL","commands":[]}"#;

        let problem = super::direct_verification_receipt_freshness_problem(
            &root,
            &receipt_path,
            receipt_text,
            &[],
            &[],
        )
        .expect("receipt should parse")
        .expect("missing Git metadata must be an explicit freshness problem");
        assert!(problem.contains("current Git HEAD"), "{problem}");

        let problem = super::direct_verification_receipt_problem(
            &root,
            &receipt_path,
            receipt_text,
            &[],
            &[],
        )
        .expect("receipt should parse")
        .expect("direct receipt inspection must propagate Git collection failure");
        assert!(problem.contains("current Git HEAD"), "{problem}");

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn direct_freshness_reports_dirty_state_collection_failure() {
        let root = temp_dir("direct-freshness-corrupt-index");
        init_git_repo(&root);
        let receipt_path = root.join(".auto/symphony/verification-receipts/TASK-STATUS-FAIL.json");
        let commit = super::current_git_commit(&root).expect("current commit");
        let plan_hash = super::current_plan_hash(&root).expect("plan hash");
        fs::write(root.join(".git/index"), "not a Git index").expect("corrupt Git index");
        let receipt_text = format!(
            r#"{{"task_id":"TASK-STATUS-FAIL","commit":"{commit}","plan_hash":"{plan_hash}","commands":[]}}"#
        );

        let problem = super::direct_verification_receipt_freshness_problem(
            &root,
            &receipt_path,
            &receipt_text,
            &[],
            &[],
        )
        .expect("receipt should parse")
        .expect("Git status failure must be an explicit freshness problem");
        assert!(problem.contains("current dirty state"), "{problem}");

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn direct_freshness_reports_plan_collection_failure() {
        let root = temp_dir("direct-freshness-missing-plan");
        init_git_repo(&root);
        let receipt_path = root.join(".auto/symphony/verification-receipts/TASK-PLAN-FAIL.json");
        let commit = super::current_git_commit(&root).expect("current commit");
        let dirty = super::current_dirty_state_fingerprint(&root).expect("current dirty state");
        let plan_hash = super::current_plan_hash(&root).expect("plan hash");
        fs::remove_file(root.join("IMPLEMENTATION_PLAN.md")).expect("remove plan");
        let receipt_text = format!(
            r#"{{"task_id":"TASK-PLAN-FAIL","commit":"{commit}","dirty_state":{{"fingerprint":"{dirty}","entries":[]}},"plan_hash":"{plan_hash}","commands":[]}}"#
        );

        let problem = super::direct_verification_receipt_freshness_problem(
            &root,
            &receipt_path,
            &receipt_text,
            &[],
            &[],
        )
        .expect("receipt should parse")
        .expect("plan collection failure must be an explicit freshness problem");
        assert!(problem.contains("current plan input"), "{problem}");

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn direct_freshness_reports_injected_collection_bound_exhaustion() {
        let root = temp_dir("direct-freshness-bound");
        init_git_repo(&root);
        let receipt_path = root.join(".auto/symphony/verification-receipts/TASK-BOUND.json");
        let receipt_text = r#"{"task_id":"TASK-BOUND","commands":[]}"#;
        let limits = super::SourceStateLimits {
            max_submodule_depth: super::SOURCE_STATE_MAX_SUBMODULE_DEPTH,
            max_entries: super::SOURCE_STATE_MAX_ENTRIES,
            max_bytes: 0,
        };

        let problem = super::direct_verification_receipt_freshness_problem_with_limits(
            &root,
            &receipt_path,
            receipt_text,
            &[],
            &[],
            limits,
        )
        .expect("receipt should parse")
        .expect("bound exhaustion must be an explicit freshness problem");
        assert!(problem.contains("byte bound"), "{problem}");
        assert!(problem.contains("current Git HEAD"), "{problem}");

        let _ = fs::remove_dir_all(root);
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
            command: format!("bash -lc '{expected}'"),
            argv: vec!["bash".to_string(), "-lc".to_string(), expected.to_string()],
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
            command: "env 'RUSTFLAGS=--cfg beta' cargo check -p demo".to_string(),
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
    fn command_matches_unwraps_task_verification_wrapper() {
        let inner = "cargo test -p rsociety-driver live_driver_generates_ludeme_forge_archive_and_portfolio_paid_readback -- --exact";
        let expected = format!(
            "scripts/run-task-verification.sh TASK-080726-EARNING-PROOF-CHECKPOINT -- {inner}"
        );
        let entry = VerificationReceiptCommand {
            command: inner.to_string(),
            argv: vec![
                "cargo".to_string(),
                "test".to_string(),
                "-p".to_string(),
                "rsociety-driver".to_string(),
                "live_driver_generates_ludeme_forge_archive_and_portfolio_paid_readback"
                    .to_string(),
                "--".to_string(),
                "--exact".to_string(),
            ],
            ..VerificationReceiptCommand::default()
        };
        assert!(super::verification_receipt_command_matches(
            &entry, &expected
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
