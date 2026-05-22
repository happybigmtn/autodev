//! Finder normalization, phase-output validation, and bug-pipeline JSON repair.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::Path;

use anyhow::{bail, Context, Result};
use serde::de::DeserializeOwned;
use serde::Deserialize;
use tokio::time::Duration;

use crate::bug_command::backend::{run_backend_prompt, LlmBackend};
use crate::bug_command::chunker::slugify;
use crate::bug_command::llm_json::{
    escape_unescaped_quotes_in_json_strings, extract_complete_json_value_prefix,
    extract_fenced_json_block, JSON_REPAIR_MAX_BYTES,
};
use crate::bug_command::prompts::build_bug_json_repair_prompt;
use crate::bug_command::types::{
    AcceptedFinding, BugFinding, BugIdRewrite, FinalReviewResult, FixResult, RepoChunk,
    ReviewResult, SkepticVerdict,
};
use crate::bug_command::BUG_CHUNK_PHASE_TIMEOUT_SECS;
use crate::util::atomic_write;

pub(crate) fn normalize_and_validate_finder_findings(
    chunk: &RepoChunk,
    findings_json_path: &Path,
    findings: Vec<BugFinding>,
) -> Result<Vec<BugFinding>> {
    let (findings, rewrites) = normalize_finder_findings(chunk, findings);
    if !rewrites.is_empty() {
        for rewrite in &rewrites {
            println!(
                "warning: normalized finder bug id `{}` -> `{}` for {}",
                rewrite.old_id, rewrite.new_id, chunk.id
            );
        }
        let json = serde_json::to_vec_pretty(&findings)
            .context("failed to serialize normalized finder findings")?;
        atomic_write(findings_json_path, &json)?;
    }

    validate_findings(chunk, &findings)?;
    Ok(findings)
}

pub(crate) fn normalize_finder_findings(
    chunk: &RepoChunk,
    mut findings: Vec<BugFinding>,
) -> (Vec<BugFinding>, Vec<BugIdRewrite>) {
    let mut rewrites = Vec::new();
    for (index, finding) in findings.iter_mut().enumerate() {
        let canonical_id = format!("BUG-{:03}-{:02}", chunk.ordinal, index + 1);
        if finding.bug_id != canonical_id {
            rewrites.push(BugIdRewrite {
                old_id: finding.bug_id.clone(),
                new_id: canonical_id.clone(),
            });
            finding.bug_id = canonical_id;
        }
    }
    (findings, rewrites)
}

pub(crate) fn validate_findings(chunk: &RepoChunk, findings: &[BugFinding]) -> Result<()> {
    for finding in findings {
        if !finding
            .bug_id
            .starts_with(&format!("BUG-{:03}-", chunk.ordinal))
        {
            bail!(
                "finder bug id `{}` does not match chunk ordinal {:03}",
                finding.bug_id,
                chunk.ordinal
            );
        }
        let impact = finding.impact.to_ascii_lowercase();
        let expected_points = match impact.as_str() {
            "low" => 1,
            "medium" => 5,
            "critical" => 10,
            other => bail!("invalid impact `{other}` in {}", finding.bug_id),
        };
        if finding.points != expected_points {
            bail!(
                "finder points mismatch in {}: expected {} for impact `{}` but found {}",
                finding.bug_id,
                expected_points,
                finding.impact,
                finding.points
            );
        }
        if finding.title.trim().is_empty()
            || finding.location.trim().is_empty()
            || finding.description.trim().is_empty()
            || finding.why_plausible.trim().is_empty()
        {
            bail!(
                "finder output for {} is missing required fields",
                finding.bug_id
            );
        }
        if finding.falsification_checks.is_empty() {
            bail!(
                "finder output for {} must include falsification checks",
                finding.bug_id
            );
        }
        if finding.evidence.is_empty() || !finding_has_grounded_evidence(chunk, finding) {
            bail!(
                "finder output for {} must include direct repo-grounded evidence",
                finding.bug_id
            );
        }
    }
    Ok(())
}

fn finding_has_grounded_evidence(chunk: &RepoChunk, finding: &BugFinding) -> bool {
    let mut haystack = finding.evidence.join("\n");
    haystack.push('\n');
    haystack.push_str(&finding.location);
    chunk.files.iter().any(|file| {
        haystack.contains(file) || haystack.contains(file.split('/').next().unwrap_or(file))
    })
}

pub(crate) fn validate_accepted_findings(
    findings: &[BugFinding],
    accepted: &[AcceptedFinding],
) -> Result<()> {
    let finding_ids = findings
        .iter()
        .map(|finding| finding.bug_id.as_str())
        .collect::<HashSet<_>>();
    let mut seen = HashSet::new();
    for finding in accepted {
        if !finding_ids.contains(finding.bug_id.as_str()) {
            bail!(
                "accepted findings contains unknown bug id `{}`",
                finding.bug_id
            );
        }
        if !seen.insert(finding.bug_id.as_str()) {
            bail!(
                "accepted findings contains duplicate bug id `{}`",
                finding.bug_id
            );
        }
    }
    Ok(())
}

pub(crate) fn derive_accepted_findings(
    chunk: &RepoChunk,
    findings: &[BugFinding],
    verdicts: &[SkepticVerdict],
) -> Result<(usize, Vec<AcceptedFinding>)> {
    validate_skeptic_verdicts(findings, verdicts)?;

    let mut verdicts_by_id = HashMap::<&str, &SkepticVerdict>::new();
    for verdict in verdicts {
        verdicts_by_id.insert(verdict.bug_id.as_str(), verdict);
    }

    let mut accepted = Vec::new();
    let mut disproved = 0usize;
    for finding in findings {
        let verdict = verdicts_by_id
            .get(finding.bug_id.as_str())
            .with_context(|| format!("skeptic output missing verdict for {}", finding.bug_id))?;
        match verdict.decision.trim().to_ascii_lowercase().as_str() {
            "accepted" => accepted.push(AcceptedFinding {
                bug_id: finding.bug_id.clone(),
                chunk_id: chunk.id.clone(),
                title: finding.title.clone(),
                location: finding.location.clone(),
                impact: finding.impact.clone(),
                points: finding.points,
                description: finding.description.clone(),
                why_plausible: finding.why_plausible.clone(),
                falsification_checks: finding.falsification_checks.clone(),
                evidence: finding.evidence.clone(),
                skeptic_confidence_percent: verdict.confidence_percent,
                skeptic_counter_argument: verdict.counter_argument.clone(),
                skeptic_follow_up_checks: verdict.follow_up_checks.clone(),
            }),
            "disproved" => disproved += 1,
            other => bail!("invalid skeptic decision `{other}` for {}", finding.bug_id),
        }
    }

    Ok((disproved, accepted))
}

fn validate_skeptic_verdicts(findings: &[BugFinding], verdicts: &[SkepticVerdict]) -> Result<()> {
    let finding_ids = findings
        .iter()
        .map(|finding| finding.bug_id.as_str())
        .collect::<HashSet<_>>();
    let mut verdict_ids = HashSet::new();

    for verdict in verdicts {
        if !finding_ids.contains(verdict.bug_id.as_str()) {
            bail!(
                "skeptic output contains unknown bug id `{}`",
                verdict.bug_id
            );
        }
        if !verdict_ids.insert(verdict.bug_id.as_str()) {
            bail!(
                "skeptic output contains duplicate verdict for `{}`",
                verdict.bug_id
            );
        }
        match verdict.decision.trim().to_ascii_lowercase().as_str() {
            "accepted" | "disproved" => {}
            other => bail!("invalid skeptic decision `{other}` for {}", verdict.bug_id),
        }
        if verdict.confidence_percent > 100 {
            bail!(
                "invalid skeptic confidence {} for {}",
                verdict.confidence_percent,
                verdict.bug_id
            );
        }
    }

    let missing = findings
        .iter()
        .filter(|finding| !verdict_ids.contains(finding.bug_id.as_str()))
        .map(|finding| finding.bug_id.as_str())
        .collect::<Vec<_>>();
    if !missing.is_empty() {
        bail!("skeptic output missing verdict(s): {}", missing.join(", "));
    }

    Ok(())
}

pub(crate) fn derive_verified_findings(
    accepted: &[AcceptedFinding],
    reviews: &[ReviewResult],
) -> Result<Vec<AcceptedFinding>> {
    let mut reviews_by_id = HashMap::<&str, &ReviewResult>::new();
    for review in reviews {
        reviews_by_id.insert(review.bug_id.as_str(), review);
    }

    let mut verified = Vec::new();
    for finding in accepted {
        let review = reviews_by_id
            .get(finding.bug_id.as_str())
            .with_context(|| format!("review output missing verdict for {}", finding.bug_id))?;
        match review.verdict.trim().to_ascii_lowercase().as_str() {
            "verified" if review.confidence.trim().eq_ignore_ascii_case("low") => {}
            "verified" => verified.push(finding.clone()),
            "discarded" => {}
            other => bail!("invalid review verdict `{other}` for {}", finding.bug_id),
        }
    }

    Ok(verified)
}

pub(crate) fn validate_fix_results(
    verified: &[AcceptedFinding],
    results: &[FixResult],
) -> Result<()> {
    validate_bug_id_coverage(
        verified.iter().map(|finding| finding.bug_id.as_str()),
        results.iter().map(|result| result.bug_id.as_str()),
        "fix results",
    )?;
    for result in results {
        match result.status.trim().to_ascii_lowercase().as_str() {
            "fixed" | "deferred" | "not_reproduced" => {}
            other => bail!("invalid fix status `{other}` for {}", result.bug_id),
        }
        if result.summary.trim().is_empty() {
            bail!("fix result for {} is missing a summary", result.bug_id);
        }
    }
    Ok(())
}

pub(crate) fn validate_final_review_results(
    verified: &[AcceptedFinding],
    results: &[FinalReviewResult],
) -> Result<()> {
    validate_bug_id_coverage(
        verified.iter().map(|finding| finding.bug_id.as_str()),
        results.iter().map(|result| result.bug_id.as_str()),
        "final review results",
    )?;
    for result in results {
        match result.status.trim().to_ascii_lowercase().as_str() {
            "confirmed" | "amended" | "deferred" => {}
            other => bail!(
                "invalid final review status `{other}` for {}",
                result.bug_id
            ),
        }
        if result.summary.trim().is_empty() {
            bail!(
                "final review result for {} is missing a summary",
                result.bug_id
            );
        }
    }
    Ok(())
}

pub(crate) fn validate_review_results(
    accepted: &[AcceptedFinding],
    results: &[ReviewResult],
) -> Result<()> {
    validate_bug_id_coverage(
        accepted.iter().map(|finding| finding.bug_id.as_str()),
        results.iter().map(|result| result.bug_id.as_str()),
        "review results",
    )?;
    for result in results {
        match result.verdict.trim().to_ascii_lowercase().as_str() {
            "verified" | "discarded" => {}
            other => bail!("invalid review verdict `{other}` for {}", result.bug_id),
        }
        match result.confidence.trim().to_ascii_lowercase().as_str() {
            "high" | "medium" | "low" => {}
            other => bail!("invalid review confidence `{other}` for {}", result.bug_id),
        }
    }
    Ok(())
}

fn validate_bug_id_coverage<'a>(
    expected: impl Iterator<Item = &'a str>,
    actual: impl Iterator<Item = &'a str>,
    label: &str,
) -> Result<()> {
    let expected = expected.collect::<Vec<_>>();
    let actual = actual.collect::<Vec<_>>();
    for bug_id in expected {
        if !actual.iter().any(|candidate| candidate == &bug_id) {
            bail!("{label} missing entry for {bug_id}");
        }
    }
    Ok(())
}

pub(crate) fn load_json_file<T>(path: &Path) -> Result<T>
where
    T: for<'de> Deserialize<'de>,
{
    let content =
        fs::read_to_string(path).with_context(|| format!("failed to read {}", path.display()))?;
    match serde_json::from_str(&content) {
        Ok(parsed) => Ok(parsed),
        Err(original_error) => {
            let repair_candidate = json_repair_candidate(&content);
            if repair_candidate.len() > JSON_REPAIR_MAX_BYTES {
                bail!(
                    "failed to parse JSON from {}: {}; automatic repair skipped because the \
candidate is {} bytes and exceeds the {}-byte limit",
                    path.display(),
                    original_error,
                    repair_candidate.len(),
                    JSON_REPAIR_MAX_BYTES
                );
            }

            if let Some(repaired) = repair_llm_json_candidate(&repair_candidate, &content) {
                match serde_json::from_str(&repaired) {
                    Ok(parsed) => {
                        println!(
                            "warning: repaired invalid or incomplete JSON in {}",
                            path.display()
                        );
                        if repaired != content {
                            atomic_write(path, repaired.as_bytes())?;
                        }
                        Ok(parsed)
                    }
                    Err(repair_error) => bail!(
                        "failed to parse JSON from {}: {}; automatic repair also failed: {}",
                        path.display(),
                        original_error,
                        repair_error
                    ),
                }
            } else {
                bail!(
                    "failed to parse JSON from {}: {}",
                    path.display(),
                    original_error
                )
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn load_json_file_with_backend_repair<T>(
    repo_root: &Path,
    path: &Path,
    backend: &LlmBackend,
    stderr_log_path: &Path,
    artifact_label: &str,
    schema_hint: &str,
    raw_response_path: &Path,
) -> Result<T>
where
    T: DeserializeOwned,
{
    match load_json_file(path) {
        Ok(parsed) => Ok(parsed),
        Err(original_error) => {
            println!(
                "warning: attempting backend repair for invalid {artifact_label} in {}",
                path.display()
            );
            attempt_llm_json_file_repair(
                repo_root,
                path,
                backend,
                stderr_log_path,
                artifact_label,
                schema_hint,
                raw_response_path,
            )
            .await
            .with_context(|| format!("backend repair failed for {}", path.display()))?;
            load_json_file(path).map_err(|repair_error| {
                anyhow::anyhow!(
                    "failed to recover {artifact_label} in {} after backend repair; original error: {}; repair error: {}",
                    path.display(),
                    original_error,
                    repair_error
                )
            })
        }
    }
}

#[cfg(test)]
fn repair_llm_json(content: &str) -> Option<String> {
    let candidate = json_repair_candidate(content);
    if candidate.len() > JSON_REPAIR_MAX_BYTES {
        return None;
    }
    repair_llm_json_candidate(&candidate, content)
}

fn json_repair_candidate(content: &str) -> String {
    extract_fenced_json_block(content).unwrap_or_else(|| content.to_string())
}

fn repair_llm_json_candidate(candidate: &str, original: &str) -> Option<String> {
    let escaped = escape_unescaped_quotes_in_json_strings(candidate);
    let candidate = extract_complete_json_value_prefix(&escaped).unwrap_or(escaped);
    let repaired = normalize_bug_pipeline_json_shapes(&candidate).unwrap_or(candidate);
    (repaired != original).then_some(repaired)
}

fn normalize_bug_pipeline_json_shapes(content: &str) -> Option<String> {
    let mut value = serde_json::from_str::<serde_json::Value>(content).ok()?;
    let repaired = normalize_bug_pipeline_value(&mut value);
    repaired
        .then(|| serde_json::to_string_pretty(&value).ok())
        .flatten()
}

fn normalize_bug_pipeline_value(value: &mut serde_json::Value) -> bool {
    let serde_json::Value::Array(entries) = value else {
        return false;
    };

    let mut repaired = false;
    for entry in entries {
        let serde_json::Value::Object(object) = entry else {
            continue;
        };

        if object.contains_key("bug_id")
            && object.contains_key("title")
            && object.contains_key("impact")
            && object.contains_key("why_plausible")
        {
            repaired |= ensure_array_field(object, "falsification_checks");
            repaired |= ensure_array_field(object, "evidence");
            continue;
        }

        if object.contains_key("bug_id")
            && object.contains_key("decision")
            && object.contains_key("confidence_percent")
        {
            repaired |= ensure_array_field(object, "follow_up_checks");
            continue;
        }

        if object.contains_key("bug_id")
            && object.contains_key("status")
            && object.contains_key("summary")
        {
            repaired |= ensure_array_field(object, "validation_commands");
            repaired |= ensure_array_field(object, "touched_files");
            repaired |= ensure_array_field(object, "residual_risks");
            continue;
        }

        if object.contains_key("bug_id")
            && object.contains_key("verdict")
            && object.contains_key("confidence")
        {
            repaired |= ensure_array_field(object, "follow_up");
            continue;
        }

        if object.contains_key("bug_id")
            && object.contains_key("chunk_id")
            && object.contains_key("skeptic_confidence_percent")
        {
            repaired |= ensure_array_field(object, "falsification_checks");
            repaired |= ensure_array_field(object, "evidence");
            repaired |= ensure_array_field(object, "skeptic_follow_up_checks");
        }
    }

    repaired
}

fn ensure_array_field(
    object: &mut serde_json::Map<String, serde_json::Value>,
    field: &str,
) -> bool {
    match object.get_mut(field) {
        None => {
            object.insert(field.to_string(), serde_json::Value::Array(Vec::new()));
            true
        }
        Some(serde_json::Value::Array(_)) => false,
        Some(serde_json::Value::Null) => {
            object.insert(field.to_string(), serde_json::Value::Array(Vec::new()));
            true
        }
        Some(serde_json::Value::String(existing)) => {
            let trimmed = existing.trim();
            let value = if trimmed.is_empty() {
                serde_json::Value::Array(Vec::new())
            } else {
                serde_json::Value::Array(vec![serde_json::Value::String(existing.clone())])
            };
            object.insert(field.to_string(), value);
            true
        }
        Some(_) => false,
    }
}

async fn attempt_llm_json_file_repair(
    repo_root: &Path,
    path: &Path,
    backend: &LlmBackend,
    stderr_log_path: &Path,
    artifact_label: &str,
    schema_hint: &str,
    raw_response_path: &Path,
) -> Result<()> {
    let prompt = build_bug_json_repair_prompt(path, raw_response_path, artifact_label, schema_hint);
    let repair_response = run_backend_prompt(
        repo_root,
        &prompt,
        backend,
        stderr_log_path,
        &format!("repair {artifact_label}"),
        Duration::from_secs(BUG_CHUNK_PHASE_TIMEOUT_SECS),
    )
    .await?;
    if !repair_response.trim().is_empty() {
        let log_path = repo_root.join(".auto").join("logs").join(format!(
            "bug-{}-repair-response.log",
            slugify(&format!(
                "{}-{}",
                artifact_label,
                path.file_stem()
                    .and_then(|value| value.to_str())
                    .unwrap_or("artifact")
            ))
        ));
        atomic_write(&log_path, repair_response.as_bytes())
            .with_context(|| format!("failed to write {}", log_path.display()))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};

    use super::{
        derive_accepted_findings, load_json_file, normalize_finder_findings, repair_llm_json,
        validate_accepted_findings, validate_findings,
    };
    use crate::bug_command::types::{AcceptedFinding, BugFinding, RepoChunk, SkepticVerdict};

    fn temp_path(name: &str) -> PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system time should be after unix epoch")
            .as_nanos();
        std::env::temp_dir().join(format!("autodev-bug-{name}-{}-{nonce}", std::process::id()))
    }

    fn test_chunk(ordinal: usize) -> RepoChunk {
        RepoChunk {
            ordinal,
            id: format!("chunk-{ordinal:03}-test"),
            scope_label: "test".to_string(),
            files: vec!["src/lib.rs".to_string()],
            risk_notes: Vec::new(),
        }
    }

    fn test_finding(bug_id: &str) -> BugFinding {
        BugFinding {
            bug_id: bug_id.to_string(),
            title: "title".to_string(),
            location: "src/lib.rs:1".to_string(),
            impact: "medium".to_string(),
            points: 5,
            description: "desc".to_string(),
            why_plausible: "why".to_string(),
            falsification_checks: vec!["check".to_string()],
            evidence: vec!["src/lib.rs:1 evidence".to_string()],
        }
    }

    #[test]
    fn normalizes_finder_bug_ids_to_chunk_order() {
        let chunk = test_chunk(3);
        let findings = vec![
            test_finding("BUG-001-01"),
            test_finding("BUG-001-04"),
            test_finding("BUG-003-09"),
        ];

        let (findings, rewrites) = normalize_finder_findings(&chunk, findings);

        let ids = findings
            .iter()
            .map(|finding| finding.bug_id.as_str())
            .collect::<Vec<_>>();
        assert_eq!(ids, vec!["BUG-003-01", "BUG-003-02", "BUG-003-03"]);
        assert_eq!(rewrites.len(), 3);
        validate_findings(&chunk, &findings).expect("normalized findings should validate");
    }

    #[test]
    fn normalizes_finder_alias_prefix_to_canonical_bug_id() {
        let chunk = test_chunk(5);
        let findings = vec![test_finding("FND-005-01")];

        let (findings, rewrites) = normalize_finder_findings(&chunk, findings);

        assert_eq!(findings[0].bug_id, "BUG-005-01");
        assert_eq!(rewrites.len(), 1);
        validate_findings(&chunk, &findings).expect("FND alias should normalize before validation");
    }

    #[test]
    fn leaves_canonical_finder_bug_ids_unchanged() {
        let chunk = test_chunk(3);
        let findings = vec![test_finding("BUG-003-01"), test_finding("BUG-003-02")];

        let (findings, rewrites) = normalize_finder_findings(&chunk, findings);

        assert!(rewrites.is_empty());
        assert_eq!(findings[0].bug_id, "BUG-003-01");
        assert_eq!(findings[1].bug_id, "BUG-003-02");
        validate_findings(&chunk, &findings).expect("canonical findings should validate");
    }

    #[test]
    fn normalization_keeps_substantive_finder_validation_strict() {
        let chunk = test_chunk(3);
        let mut finding = test_finding("BUG-001-01");
        finding.points = 10;

        let (findings, rewrites) = normalize_finder_findings(&chunk, vec![finding]);

        assert_eq!(rewrites.len(), 1);
        let err = validate_findings(&chunk, &findings).expect_err("points mismatch should fail");
        assert!(err.to_string().contains("finder points mismatch"));
    }

    #[test]
    fn accepted_findings_must_reference_known_bug_ids() {
        let findings = vec![BugFinding {
            bug_id: "BUG-001-01".to_string(),
            title: "title".to_string(),
            location: "path:1".to_string(),
            impact: "medium".to_string(),
            points: 5,
            description: "desc".to_string(),
            why_plausible: "why".to_string(),
            falsification_checks: vec!["check".to_string()],
            evidence: vec!["path:1 evidence".to_string()],
        }];
        let accepted = vec![AcceptedFinding {
            bug_id: "BUG-999-01".to_string(),
            chunk_id: "chunk-001-root".to_string(),
            title: "title".to_string(),
            location: "path:1".to_string(),
            impact: "medium".to_string(),
            points: 5,
            description: "desc".to_string(),
            why_plausible: "why".to_string(),
            falsification_checks: vec!["check".to_string()],
            evidence: vec!["evidence".to_string()],
            skeptic_confidence_percent: 90,
            skeptic_counter_argument: "counter".to_string(),
            skeptic_follow_up_checks: vec!["follow-up".to_string()],
        }];

        assert!(validate_accepted_findings(&findings, &accepted).is_err());
    }

    #[test]
    fn skeptic_verdicts_must_cover_every_finding() {
        let chunk = test_chunk(7);
        let findings = vec![test_finding("BUG-007-01"), test_finding("BUG-007-02")];
        let verdicts = vec![SkepticVerdict {
            bug_id: "BUG-007-02".to_string(),
            decision: "disproved".to_string(),
            confidence_percent: 95,
            counter_argument: "The second finding does not survive challenge.".to_string(),
            risk_calculation: "Low risk.".to_string(),
            follow_up_checks: vec!["Review the source evidence.".to_string()],
        }];

        let err = derive_accepted_findings(&chunk, &findings, &verdicts)
            .expect_err("missing skeptic verdict should invalidate the phase output");

        let message = err.to_string();
        assert!(message.contains("skeptic output missing verdict(s): BUG-007-01"));
    }

    #[test]
    fn repairs_trailing_backend_wrapper_after_json_array() {
        let invalid = r#"[
  {
    "bug_id": "BUG-011-01",
    "decision": "accepted",
    "confidence_percent": 95,
    "counter_argument": "The generated verdict is valid JSON.",
    "risk_calculation": "The backend wrapper should not abort the run.",
    "follow_up_checks": ["Resume the bug pipeline"]
  }
]
</invoke>"#;

        assert!(serde_json::from_str::<Vec<SkepticVerdict>>(invalid).is_err());

        let repaired = repair_llm_json(invalid).expect("repair should trim backend wrapper");
        assert!(!repaired.contains("</invoke>"));
        let parsed = serde_json::from_str::<Vec<SkepticVerdict>>(&repaired)
            .expect("repaired JSON should parse");
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].bug_id, "BUG-011-01");
    }

    #[test]
    fn repairs_missing_bug_finding_evidence_field() {
        let invalid = r#"[
  {
    "bug_id": "BUG-008-02",
    "title": "Missing evidence field should be repaired",
    "location": "services/home-miner-daemon/tests/test_launch_wallets.py",
    "impact": "medium",
    "points": 5,
    "description": "One finding omitted the evidence array entirely.",
    "why_plausible": "The JSON is otherwise valid and should not abort the run.",
    "falsification_checks": ["Inspect the generated finder output"]
  }
]"#;

        assert!(serde_json::from_str::<Vec<BugFinding>>(invalid).is_err());

        let repaired = repair_llm_json(invalid).expect("repair should add missing evidence");
        let parsed =
            serde_json::from_str::<Vec<BugFinding>>(&repaired).expect("repaired JSON should parse");
        assert_eq!(parsed.len(), 1);
        assert!(parsed[0].evidence.is_empty());
        assert_eq!(parsed[0].falsification_checks.len(), 1);
    }

    #[test]
    fn oversized_invalid_json_skips_automatic_repair() {
        let path = temp_path("oversized-json").join("finder-response.json");
        fs::create_dir_all(path.parent().expect("temp file should have a parent"))
            .expect("failed to create temp dir");
        let repeated_quotes = "\"broken\" ".repeat(40_000);
        let invalid = format!(
            "[{{\"bug_id\":\"BUG-001-01\",\"decision\":\"disproved\",\"confidence_percent\":95,\
\"counter_argument\":\"{repeated_quotes}\",\"risk_calculation\":\"low\",\"follow_up_checks\":[\"check\"]}}]"
        );
        fs::write(&path, invalid).expect("failed to write oversized invalid json");

        let error = load_json_file::<Vec<SkepticVerdict>>(&path)
            .expect_err("oversized invalid JSON should not attempt automatic repair");
        let message = error.to_string();
        assert!(message.contains("automatic repair skipped"));
        assert!(message.contains("exceeds"));
    }
}
