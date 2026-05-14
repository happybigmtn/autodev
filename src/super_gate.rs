//! Deterministic stage gates derived from typed JSON artifacts.
//!
//! Replaces LLM-driven gate checks with structural reads of `super-findings.json`
//! and `AUDIT-FINDINGS-SUMMARY.json`. Gates that previously cost a model call per
//! stage transition now cost a JSON parse.
//!
//! These gates are informational for v1 -- callers surface their findings as
//! warnings but do not short-circuit the LLM execution-gate pass. Once the
//! schemas are trusted (and the host has shipped a few clean runs), the gate
//! status can be promoted to a hard block.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::audit_everything::AuditFindingsSummary;
use crate::super_command::{SuperBlocker, SuperFindings};

const SUPER_FINDINGS_FILE: &str = "super-findings.json";
const AUDIT_SUMMARY_RELATIVE: &str = "harvest/AUDIT-FINDINGS-SUMMARY.json";
const SKIPPED_FILE: &str = "skipped.json";
const COVERAGE_FILE: &str = "coverage.json";

/// Outcome of a single gate evaluation. `reasons` is human-readable text for
/// the operator log; `bitrot` and `deferred` are populated by gates that
/// surface those signals (others leave them empty).
#[derive(Clone, Debug)]
pub(crate) struct GateOutcome {
    pub status: GateStatus,
    pub reasons: Vec<String>,
    pub bitrot: Vec<String>,
    pub deferred: Vec<String>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum GateStatus {
    Go,
    ConditionalGo,
    NoGo,
}

/// Read `super-findings.json` from a super run directory.
pub(crate) fn read_super_findings(super_root: &Path) -> Result<SuperFindings> {
    let path = super_root.join(SUPER_FINDINGS_FILE);
    let text =
        fs::read_to_string(&path).with_context(|| format!("failed to read {}", path.display()))?;
    serde_json::from_str(&text).with_context(|| format!("failed to parse {}", path.display()))
}

/// Read `harvest/AUDIT-FINDINGS-SUMMARY.json` from an audit run directory.
pub(crate) fn read_audit_findings_summary(audit_run_root: &Path) -> Result<AuditFindingsSummary> {
    let path = audit_run_root.join(AUDIT_SUMMARY_RELATIVE);
    let text =
        fs::read_to_string(&path).with_context(|| format!("failed to read {}", path.display()))?;
    serde_json::from_str(&text).with_context(|| format!("failed to parse {}", path.display()))
}

/// Severity-count gate. Zero `severity == "high"` blockers => `Go`. Any high
/// blockers => `NoGo` with a reason per blocker. Empty findings (no blockers)
/// is also `Go`. Conditional-go is reserved for the typed-deferred case
/// (operator-queue entries still parked); we surface them via `deferred`
/// without promoting status so v1 stays informational.
pub(crate) fn severity_count_gate(findings: &SuperFindings) -> GateOutcome {
    let high: Vec<&SuperBlocker> = findings
        .blockers
        .iter()
        .filter(|b| is_high_severity(&b.severity))
        .collect();

    let deferred: Vec<String> = findings
        .operator_queue
        .iter()
        .map(|entry| format!("{}: {}", entry.id, entry.title))
        .collect();

    if high.is_empty() {
        let status = if deferred.is_empty() {
            GateStatus::Go
        } else {
            GateStatus::ConditionalGo
        };
        return GateOutcome {
            status,
            reasons: Vec::new(),
            bitrot: Vec::new(),
            deferred,
        };
    }

    let reasons: Vec<String> = high
        .iter()
        .map(|b| format!("{} [{}] {}", b.id, b.severity, b.title))
        .collect();
    GateOutcome {
        status: GateStatus::NoGo,
        reasons,
        bitrot: Vec::new(),
        deferred,
    }
}

fn is_high_severity(value: &str) -> bool {
    matches!(value.trim().to_ascii_lowercase().as_str(), "high" | "critical" | "blocker")
}

/// Compute the blocker-bitrot delta across two consecutive super runs. Returns
/// blocker IDs that are present in BOTH runs (i.e. the same ID survived the
/// run). Output is sorted for stable display.
pub(crate) fn compute_blocker_bitrot(
    prev: &SuperFindings,
    curr: &SuperFindings,
) -> Vec<String> {
    let prev_ids: BTreeSet<&str> =
        prev.blockers.iter().map(|b| b.id.as_str()).collect();
    let curr_ids: BTreeSet<&str> =
        curr.blockers.iter().map(|b| b.id.as_str()).collect();
    prev_ids
        .intersection(&curr_ids)
        .map(|s| (*s).to_string())
        .collect()
}

/// Coverage report emitted next to the super run. Captures how many tracked
/// files the audit pass saw vs. how many it skipped, and groups the skips by
/// reason so the operator can spot allowlist drift quickly.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct CoverageReport {
    pub total_files_seen: usize,
    pub skipped_count: usize,
    pub skip_reasons: BTreeMap<String, usize>,
}

#[derive(Deserialize)]
struct SkippedEntry {
    #[allow(dead_code)]
    path: String,
    reason: String,
}

/// Build a coverage report from an audit run's `skipped.json`. The audit run
/// root is the directory that contains `manifest.json` / `skipped.json` (i.e.
/// `.auto/audit-everything/<run-id>`). When `skipped.json` is absent the report
/// is empty (0 total, 0 skipped, no reasons), not an error -- audits that ran
/// before this artifact existed should still produce a coverage row.
pub(crate) fn build_coverage_report(audit_run_root: &Path) -> Result<CoverageReport> {
    let path = audit_run_root.join(SKIPPED_FILE);
    if !path.exists() {
        return Ok(CoverageReport {
            total_files_seen: 0,
            skipped_count: 0,
            skip_reasons: BTreeMap::new(),
        });
    }
    let text =
        fs::read_to_string(&path).with_context(|| format!("failed to read {}", path.display()))?;
    let entries: Vec<SkippedEntry> = serde_json::from_str(&text)
        .with_context(|| format!("failed to parse {}", path.display()))?;
    let skipped_count = entries.len();
    let mut skip_reasons: BTreeMap<String, usize> = BTreeMap::new();
    for entry in &entries {
        *skip_reasons.entry(entry.reason.clone()).or_insert(0) += 1;
    }
    // The "total files seen" derives from the audit's manifest.json when
    // present; otherwise fall back to the skipped count so the row remains
    // honest about its provenance.
    let total_files_seen = read_manifest_file_count(audit_run_root).unwrap_or(skipped_count);
    Ok(CoverageReport {
        total_files_seen,
        skipped_count,
        skip_reasons,
    })
}

/// Write a coverage report to `<super_root>/coverage.json`.
pub(crate) fn write_coverage_report(super_root: &Path, report: &CoverageReport) -> Result<PathBuf> {
    fs::create_dir_all(super_root)
        .with_context(|| format!("failed to create {}", super_root.display()))?;
    let path = super_root.join(COVERAGE_FILE);
    let serialized = serde_json::to_vec_pretty(report)
        .with_context(|| format!("failed to serialize {}", path.display()))?;
    crate::util::atomic_write(&path, &serialized)
        .with_context(|| format!("failed to write {}", path.display()))?;
    Ok(path)
}

fn read_manifest_file_count(audit_run_root: &Path) -> Option<usize> {
    let path = audit_run_root.join("manifest.json");
    let text = fs::read_to_string(&path).ok()?;
    let value: serde_json::Value = serde_json::from_str(&text).ok()?;
    value
        .get("files")
        .and_then(|v| v.as_array())
        .map(|arr| arr.len())
}

/// Walk `.auto/super/` and return the most recent run directory whose timestamp
/// slug sorts strictly before `current_super_root`'s basename. Returns `None`
/// when no older run exists.
pub(crate) fn find_prior_super_run(
    repo_root: &Path,
    current_super_root: &Path,
) -> Option<PathBuf> {
    let super_dir = repo_root.join(".auto").join("super");
    let current_name = current_super_root.file_name()?.to_str()?.to_string();
    let entries = fs::read_dir(&super_dir).ok()?;
    let mut candidates: Vec<(String, PathBuf)> = Vec::new();
    for entry in entries.flatten() {
        let file_type = match entry.file_type() {
            Ok(ft) => ft,
            Err(_) => continue,
        };
        if !file_type.is_dir() {
            continue;
        }
        let name = match entry.file_name().into_string() {
            Ok(s) => s,
            Err(_) => continue,
        };
        if name == current_name {
            continue;
        }
        if name >= current_name {
            continue;
        }
        candidates.push((name, entry.path()));
    }
    candidates.sort_by(|a, b| a.0.cmp(&b.0));
    candidates.pop().map(|(_, path)| path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::super_command::{SuperBlocker, SuperCampaignPlan, SuperFindings};
    use std::sync::atomic::{AtomicU64, Ordering};

    static TEMP_SEQ: AtomicU64 = AtomicU64::new(0);

    fn tempdir(label: &str) -> PathBuf {
        let seq = TEMP_SEQ.fetch_add(1, Ordering::SeqCst);
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let base = std::env::temp_dir().join(format!(
            "autodev-super-gate-{label}-{}-{nanos}-{seq}",
            std::process::id()
        ));
        fs::create_dir_all(&base).expect("mkdir tempdir");
        base
    }

    fn cleanup(p: &Path) {
        let _ = fs::remove_dir_all(p);
    }

    fn blocker(id: &str, severity: &str) -> SuperBlocker {
        SuperBlocker {
            id: id.to_string(),
            title: format!("title for {id}"),
            owner_surface: "owner".to_string(),
            severity: severity.to_string(),
            evidence: "ev".to_string(),
            remediation_hint: "hint".to_string(),
        }
    }

    fn findings_with(blockers: Vec<SuperBlocker>) -> SuperFindings {
        SuperFindings {
            run_id: "test".to_string(),
            generated_at: "now".to_string(),
            readiness: "amber".to_string(),
            blockers,
            risks: Vec::new(),
            gates: Vec::new(),
            campaign_plan: SuperCampaignPlan {
                horizon_days: 14,
                milestones: Vec::new(),
            },
            operator_queue: Vec::new(),
            auto_resolved: Vec::new(),
        }
    }

    #[test]
    fn severity_gate_go_on_empty_blockers() {
        let outcome = severity_count_gate(&findings_with(Vec::new()));
        assert_eq!(outcome.status, GateStatus::Go);
        assert!(outcome.reasons.is_empty());
    }

    #[test]
    fn severity_gate_go_on_only_low_blockers() {
        let outcome = severity_count_gate(&findings_with(vec![
            blocker("BLK-001", "low"),
            blocker("BLK-002", "medium"),
        ]));
        assert_eq!(outcome.status, GateStatus::Go);
        assert!(outcome.reasons.is_empty());
    }

    #[test]
    fn severity_gate_no_go_on_any_high() {
        let outcome = severity_count_gate(&findings_with(vec![
            blocker("BLK-001", "low"),
            blocker("BLK-002", "High"),
            blocker("BLK-003", "blocker"),
        ]));
        assert_eq!(outcome.status, GateStatus::NoGo);
        assert_eq!(outcome.reasons.len(), 2);
        assert!(outcome.reasons[0].contains("BLK-002"));
        assert!(outcome.reasons[1].contains("BLK-003"));
    }

    #[test]
    fn severity_gate_conditional_go_when_queue_nonempty() {
        let mut findings = findings_with(vec![blocker("BLK-001", "low")]);
        findings
            .operator_queue
            .push(crate::super_command::OperatorQueueEntry {
                id: "OQ-1".to_string(),
                title: "needs human".to_string(),
                policy: crate::super_command::OperatorPolicy::External,
                resolver_kind: None,
                payload: String::new(),
                evidence: String::new(),
            });
        let outcome = severity_count_gate(&findings);
        assert_eq!(outcome.status, GateStatus::ConditionalGo);
        assert_eq!(outcome.deferred.len(), 1);
        assert!(outcome.deferred[0].contains("OQ-1"));
    }

    #[test]
    fn blocker_bitrot_finds_intersection_only() {
        let prev = findings_with(vec![
            blocker("BLK-001", "high"),
            blocker("BLK-002", "high"),
            blocker("BLK-003", "low"),
        ]);
        let curr = findings_with(vec![
            blocker("BLK-002", "high"),
            blocker("BLK-003", "low"),
            blocker("BLK-004", "high"),
        ]);
        let bitrot = compute_blocker_bitrot(&prev, &curr);
        assert_eq!(bitrot, vec!["BLK-002".to_string(), "BLK-003".to_string()]);
    }

    #[test]
    fn blocker_bitrot_empty_when_no_overlap() {
        let prev = findings_with(vec![blocker("BLK-001", "high")]);
        let curr = findings_with(vec![blocker("BLK-002", "high")]);
        let bitrot = compute_blocker_bitrot(&prev, &curr);
        assert!(bitrot.is_empty());
    }

    #[test]
    fn coverage_report_missing_skipped_returns_empty() {
        let tmp = tempdir("missing-skipped");
        let report = build_coverage_report(&tmp).expect("coverage");
        assert_eq!(report.total_files_seen, 0);
        assert_eq!(report.skipped_count, 0);
        assert!(report.skip_reasons.is_empty());
        cleanup(&tmp);
    }

    #[test]
    fn coverage_report_groups_reasons() {
        let tmp = tempdir("group-reasons");
        let payload = serde_json::json!([
            {"path": "a.bin", "reason": "binary"},
            {"path": "b.bin", "reason": "binary"},
            {"path": "c.png", "reason": "image"},
            {"path": "d.lock", "reason": "lockfile"},
        ]);
        let skipped_path = tmp.join(SKIPPED_FILE);
        fs::write(&skipped_path, serde_json::to_vec_pretty(&payload).unwrap()).unwrap();
        let report = build_coverage_report(&tmp).expect("coverage");
        assert_eq!(report.skipped_count, 4);
        assert_eq!(report.skip_reasons.get("binary").copied(), Some(2));
        assert_eq!(report.skip_reasons.get("image").copied(), Some(1));
        assert_eq!(report.skip_reasons.get("lockfile").copied(), Some(1));
        // No manifest -> total_files_seen falls back to skipped_count.
        assert_eq!(report.total_files_seen, 4);
        cleanup(&tmp);
    }

    #[test]
    fn coverage_report_uses_manifest_file_count_when_present() {
        let tmp = tempdir("manifest-count");
        let skipped = serde_json::json!([
            {"path": "x", "reason": "binary"},
        ]);
        fs::write(
            tmp.join(SKIPPED_FILE),
            serde_json::to_vec_pretty(&skipped).unwrap(),
        )
        .unwrap();
        let manifest = serde_json::json!({
            "files": [
                {"path": "x"},
                {"path": "y"},
                {"path": "z"},
            ]
        });
        fs::write(
            tmp.join("manifest.json"),
            serde_json::to_vec_pretty(&manifest).unwrap(),
        )
        .unwrap();
        let report = build_coverage_report(&tmp).expect("coverage");
        assert_eq!(report.total_files_seen, 3);
        assert_eq!(report.skipped_count, 1);
        cleanup(&tmp);
    }

    #[test]
    fn find_prior_super_run_picks_most_recent_older() {
        let tmp = tempdir("prior-recent");
        let super_dir = tmp.join(".auto").join("super");
        fs::create_dir_all(&super_dir).unwrap();
        for slug in ["20260101-000000", "20260201-000000", "20260301-000000"] {
            fs::create_dir_all(super_dir.join(slug)).unwrap();
        }
        let current = super_dir.join("20260301-000000");
        let prior = find_prior_super_run(&tmp, &current).expect("prior");
        assert!(prior.ends_with("20260201-000000"));
        cleanup(&tmp);
    }

    #[test]
    fn find_prior_super_run_returns_none_when_only_current() {
        let tmp = tempdir("prior-none");
        let super_dir = tmp.join(".auto").join("super");
        fs::create_dir_all(super_dir.join("20260301-000000")).unwrap();
        let current = super_dir.join("20260301-000000");
        assert!(find_prior_super_run(&tmp, &current).is_none());
        cleanup(&tmp);
    }
}
