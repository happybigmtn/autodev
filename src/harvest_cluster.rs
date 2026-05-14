//! Pre-LLM clustering + complexity classification for audit harvest.
//!
//! Single conceptual fixes (e.g. bridge-key redaction touching 50 files) used
//! to fan out into ~50 IMPLEMENTATION_PLAN rows -- one per file -- which was
//! the dominant noise source in the harvest. This module groups findings by
//! shared path-ancestor + class + title signature BEFORE the model is asked
//! to author task rows, and tags each group with a complexity class that
//! routes it to the right downstream artifact (plan row, design-doc queue,
//! or operator queue).
//!
//! Lives as a `#[path = "harvest_cluster.rs"] mod` child of `super_command`
//! so we keep the canonical sibling source layout without touching `main.rs`.
//!
//! Consumers:
//! - `super_command::dispatch_classified_harvest` routes by `ComplexityClass`.
//! - `super_command::build_audit_harvest_prompt` hands the model the cluster
//!   summary instead of one row per file.
//!
//! Input is the typed `AuditFinding` written by `audit_everything::collect_audit_findings`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::audit_everything::{AuditFinding, FindingClass};

/// Stable identity of a cluster. Two findings collapse into one group iff their
/// `ClusterKey`s match exactly.
#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Hash, Serialize, Deserialize)]
pub(crate) struct ClusterKey {
    pub(crate) path_ancestor: PathBuf,
    pub(crate) finding_class: String,
    pub(crate) signature_hash: String,
}

/// Group of findings collapsed by `ClusterKey`. The seed is the
/// lowest-dr-id finding in the group; the cluster path is the longest
/// common path ancestor across all member paths (or the seed path when
/// only one path is present).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct ClusterGroup {
    pub(crate) key: ClusterKey,
    pub(crate) seed: AuditFinding,
    pub(crate) cluster_title: String,
    pub(crate) cluster_path: String,
    pub(crate) dedup_keys: Vec<String>,
    pub(crate) member_paths: Vec<String>,
    pub(crate) member_count: usize,
}

impl ClusterGroup {
    pub(crate) fn directories_touched(&self) -> usize {
        let mut dirs: std::collections::BTreeSet<&str> = std::collections::BTreeSet::new();
        for path in &self.member_paths {
            let dir = match path.rfind('/') {
                Some(idx) => &path[..idx],
                None => "",
            };
            dirs.insert(dir);
        }
        dirs.len()
    }
}

/// Downstream routing decision for a cluster. SingleRow flows through the
/// existing harvest -> IMPLEMENTATION_PLAN.md path; the other three classes
/// are parked in dedicated queues that operators triage separately.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ComplexityClass {
    SingleRow,
    CrossCuttingRefactor,
    GeneratorLevel,
    ExternalState,
}

impl ComplexityClass {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            ComplexityClass::SingleRow => "single_row",
            ComplexityClass::CrossCuttingRefactor => "cross_cutting_refactor",
            ComplexityClass::GeneratorLevel => "generator_level",
            ComplexityClass::ExternalState => "external_state",
        }
    }
}

/// Cluster `findings` by `(class, title-signature)`, then compute the
/// shared `path_ancestor` across each bucket's members. Two findings with
/// the same root cause but different directories still collapse into one
/// group; the cluster's ancestor reflects the breadth of that footprint.
pub(crate) fn cluster_findings(findings: &[AuditFinding]) -> Vec<ClusterGroup> {
    let mut buckets: BTreeMap<(String, String), Vec<&AuditFinding>> = BTreeMap::new();
    for finding in findings {
        let class = finding.class.as_str().to_string();
        let signature = signature_hash(finding);
        buckets.entry((class, signature)).or_default().push(finding);
    }

    let mut groups: Vec<ClusterGroup> = Vec::with_capacity(buckets.len());
    for ((class, signature), mut members) in buckets {
        members.sort_by(|left, right| left.dr_id.cmp(&right.dr_id));
        let seed = members[0].clone();
        let mut member_paths: Vec<String> = members
            .iter()
            .flat_map(|finding| finding.paths.iter().cloned())
            .collect();
        member_paths.sort();
        member_paths.dedup();
        let path_ancestor = longest_common_path_ancestor(&member_paths);
        let key = ClusterKey {
            path_ancestor: path_ancestor.clone(),
            finding_class: class,
            signature_hash: signature,
        };
        let ancestor_display = path_ancestor.display().to_string();
        let cluster_path = if member_paths.is_empty() {
            ancestor_display.clone()
        } else if member_paths.len() == 1 {
            member_paths[0].clone()
        } else if ancestor_display.is_empty() {
            format!("{} paths spanning repo root", member_paths.len())
        } else {
            format!("{ancestor_display}/**")
        };
        let dedup_keys: Vec<String> = members
            .iter()
            .map(|finding| finding.dedup_key.clone())
            .collect();
        let cluster_title = synthesize_cluster_title(&seed, members.len());
        groups.push(ClusterGroup {
            key,
            seed,
            cluster_title,
            cluster_path,
            dedup_keys,
            member_paths,
            member_count: members.len(),
        });
    }

    groups.sort_by(|left, right| left.seed.dr_id.cmp(&right.seed.dr_id));
    groups
}

/// Classify a cluster by the policy described in the change doc.
pub(crate) fn classify_complexity(group: &ClusterGroup) -> ComplexityClass {
    if mentions_external_state(&group.seed) {
        return ComplexityClass::ExternalState;
    }
    if group
        .member_paths
        .iter()
        .any(|path| is_generator_path(path))
    {
        return ComplexityClass::GeneratorLevel;
    }
    if group.member_paths.len() > 5 {
        return ComplexityClass::CrossCuttingRefactor;
    }
    if group.directories_touched() > 2 && group.member_paths.len() >= 2 {
        return ComplexityClass::CrossCuttingRefactor;
    }
    if group.member_paths.len() == 1 && group.seed.class != FindingClass::None {
        return ComplexityClass::SingleRow;
    }
    if group.member_paths.is_empty() {
        return ComplexityClass::SingleRow;
    }
    ComplexityClass::SingleRow
}

fn mentions_external_state(finding: &AuditFinding) -> bool {
    let hint = finding.complexity_hint.to_ascii_lowercase();
    if hint.contains("external-state") || hint.contains("external state") {
        return true;
    }
    let missing = finding.proof_missing.to_ascii_lowercase();
    let signals = [
        "wall-clock",
        "wall clock",
        "pool acceptance",
        "runtime data",
        "live runtime",
        "production traffic",
    ];
    signals.iter().any(|needle| missing.contains(needle))
}

fn is_generator_path(path: &str) -> bool {
    let normalized = path.trim_start_matches("./");
    if normalized.ends_with("build.rs") {
        return true;
    }
    for marker in ["/generated/", "/codegen/", "/proto/"] {
        if normalized.contains(marker) || normalized.starts_with(marker.trim_start_matches('/')) {
            return true;
        }
    }
    // `*.gen.*` (e.g. `schema.gen.rs`, `client.gen.ts`).
    let file_name = normalized
        .rsplit('/')
        .next()
        .unwrap_or(normalized)
        .to_ascii_lowercase();
    let parts: Vec<&str> = file_name.split('.').collect();
    parts.iter().any(|part| *part == "gen")
}

/// `sha256` of `class:title-stem`. The title-stem strips path-specific tokens
/// so two findings with the same root cause but different file references
/// produce the same hash (`Redact bridge key in src/foo.rs` and
/// `Redact bridge key in src/bar.rs` collapse).
fn signature_hash(finding: &AuditFinding) -> String {
    let stem = title_stem(&finding.title);
    let payload = format!("{}:{}", finding.class.as_str(), stem);
    let mut hasher = Sha256::new();
    hasher.update(payload.as_bytes());
    format!("{:x}", hasher.finalize())
}

fn title_stem(title: &str) -> String {
    // 1. Strip path-like tokens whole (anything with a `/`, or a recognized
    //    file-name pattern like `foo.rs`). The original title is preserved
    //    through this step so we don't accidentally fuse identifiers.
    let pre_scrub = scrub_path_tokens(title);

    // 2. Tokenize on non-alphanumeric, strip per-token noise (digit-only
    //    identifiers, trailing digit suffixes used as file numbering).
    let lower = pre_scrub.to_ascii_lowercase();
    let mut out: Vec<String> = Vec::new();
    for raw in lower.split(|ch: char| !ch.is_ascii_alphanumeric() && ch != '_') {
        let token = raw.trim_matches('`').trim_matches('_');
        if token.is_empty() {
            continue;
        }
        if looks_like_numeric_noise(token) {
            continue;
        }
        let stripped = strip_trailing_digit_suffix(token);
        if stripped.is_empty() {
            continue;
        }
        out.push(stripped.to_string());
    }
    out.join(" ")
}

/// Remove tokens that contain a `/`, or that look like a filename with a
/// recognized extension. Preserves intervening words so the remaining title
/// still describes the conceptual fix.
fn scrub_path_tokens(title: &str) -> String {
    title
        .split_whitespace()
        .filter(|token| !looks_like_path_or_filename(token))
        .collect::<Vec<&str>>()
        .join(" ")
}

fn looks_like_path_or_filename(raw: &str) -> bool {
    let token = raw.trim_matches(|ch: char| matches!(ch, '`' | '"' | '\'' | '.' | ',' | ';' | ':'));
    if token.is_empty() {
        return false;
    }
    if token.contains('/') || token.contains('\\') {
        return true;
    }
    let lower = token.to_ascii_lowercase();
    for ext in [
        ".rs", ".md", ".toml", ".json", ".ts", ".tsx", ".js", ".jsx", ".py", ".sh", ".yaml",
        ".yml", ".html", ".css", ".svg", ".txt", ".sql", ".move",
    ] {
        if lower.ends_with(ext) && lower.len() > ext.len() {
            return true;
        }
    }
    false
}

fn looks_like_numeric_noise(token: &str) -> bool {
    token.chars().all(|ch| ch.is_ascii_digit()) && token.len() <= 6
}

/// Strip a trailing run of digits from a token so `file0`/`file1`/`route23`
/// all collapse to `file`/`route`. Identifiers that are *entirely* digits
/// are caught by `looks_like_numeric_noise`.
fn strip_trailing_digit_suffix(token: &str) -> &str {
    let end = token
        .char_indices()
        .rev()
        .take_while(|(_, ch)| ch.is_ascii_digit())
        .last()
        .map(|(idx, _)| idx)
        .unwrap_or(token.len());
    if end == 0 {
        return token;
    }
    if end == token.len() {
        return token;
    }
    &token[..end]
}

/// Longest path prefix shared by every member path. Empty paths produce an
/// empty ancestor (cluster falls back to repo root semantics).
pub(crate) fn longest_common_path_ancestor(paths: &[String]) -> PathBuf {
    if paths.is_empty() {
        return PathBuf::new();
    }
    let mut iter = paths.iter().map(|p| split_path_segments(p));
    let mut common: Vec<String> = match iter.next() {
        Some(first) => first,
        None => return PathBuf::new(),
    };
    for segments in iter {
        let cap = common.len().min(segments.len());
        common.truncate(cap);
        for idx in 0..cap {
            if common[idx] != segments[idx] {
                common.truncate(idx);
                break;
            }
        }
        if common.is_empty() {
            break;
        }
    }
    // Drop the trailing file segment if present so the ancestor is a directory.
    if let Some(last) = common.last() {
        if last.contains('.') && !last.starts_with('.') {
            common.pop();
        }
    }
    let mut buf = PathBuf::new();
    for segment in common {
        buf.push(segment);
    }
    buf
}

fn split_path_segments(path: &str) -> Vec<String> {
    let trimmed = path.trim_start_matches("./");
    Path::new(trimmed)
        .components()
        .filter_map(|component| component.as_os_str().to_str().map(str::to_string))
        .collect()
}

fn synthesize_cluster_title(seed: &AuditFinding, member_count: usize) -> String {
    if member_count <= 1 {
        return seed.title.clone();
    }
    format!(
        "{} (cluster of {} findings across {})",
        seed.title.trim(),
        member_count,
        seed.class.as_str()
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audit_everything::{audit_finding_dedup_key, FindingClass};

    fn finding(
        dr_id: &str,
        class: FindingClass,
        title: &str,
        cluster: &str,
        paths: &[&str],
        complexity_hint: &str,
        proof_missing: &str,
    ) -> AuditFinding {
        let paths: Vec<String> = paths.iter().map(|p| (*p).to_string()).collect();
        let dedup_key = audit_finding_dedup_key(cluster, class, &paths);
        AuditFinding {
            dr_id: dr_id.to_string(),
            title: title.to_string(),
            cluster: cluster.to_string(),
            paths,
            class,
            complexity_hint: complexity_hint.to_string(),
            proof_found: String::new(),
            proof_missing: proof_missing.to_string(),
            risk: "med".to_string(),
            dedup_key,
        }
    }

    #[test]
    fn cluster_collapses_bridge_key_fan_out_into_one_group() {
        let findings: Vec<AuditFinding> = (0..50)
            .map(|idx| {
                finding(
                    &format!("DR-{idx:03}"),
                    FindingClass::Consolidate,
                    &format!("Redact bridge key in web/src/routes/file{idx}.rs"),
                    "bridge_key_redaction",
                    &[&format!("web/src/routes/file{idx}.rs")],
                    "single-row",
                    "",
                )
            })
            .collect();

        let clusters = cluster_findings(&findings);
        assert_eq!(
            clusters.len(),
            1,
            "expected 50 bridge-key findings to collapse into one cluster"
        );
        assert_eq!(clusters[0].member_count, 50);
        assert!(clusters[0]
            .cluster_path
            .starts_with("web/src/routes")
            || clusters[0].cluster_path.contains("web/src/routes"));
    }

    #[test]
    fn classify_single_row_for_one_path() {
        let single = finding(
            "DR-001",
            FindingClass::Simplify,
            "Inline duplicate match arm in src/util.rs",
            "util_match_arm",
            &["src/util.rs"],
            "single-row",
            "",
        );
        let cluster = &cluster_findings(&[single])[0];
        assert_eq!(classify_complexity(cluster), ComplexityClass::SingleRow);
    }

    #[test]
    fn classify_cross_cutting_when_paths_exceed_five() {
        let findings: Vec<AuditFinding> = (0..7)
            .map(|idx| {
                finding(
                    &format!("DR-{idx:03}"),
                    FindingClass::Consolidate,
                    &format!("Drift in src/foo{idx}.rs"),
                    "drift",
                    &[&format!("src/foo{idx}.rs")],
                    "single-row",
                    "",
                )
            })
            .collect();
        let clusters = cluster_findings(&findings);
        let touched_paths: usize = clusters.iter().map(|g| g.member_paths.len()).sum();
        assert_eq!(touched_paths, 7);
        let by_complexity: Vec<_> = clusters.iter().map(classify_complexity).collect();
        assert!(by_complexity.contains(&ComplexityClass::CrossCuttingRefactor));
    }

    #[test]
    fn classify_cross_cutting_when_three_directories_involved() {
        let findings = [
            finding(
                "DR-001",
                FindingClass::Simplify,
                "Same fix",
                "shared_root",
                &["a/one.rs"],
                "single-row",
                "",
            ),
            finding(
                "DR-002",
                FindingClass::Simplify,
                "Same fix",
                "shared_root",
                &["b/two.rs"],
                "single-row",
                "",
            ),
            finding(
                "DR-003",
                FindingClass::Simplify,
                "Same fix",
                "shared_root",
                &["c/three.rs"],
                "single-row",
                "",
            ),
        ];
        let clusters = cluster_findings(&findings);
        assert_eq!(clusters.len(), 1);
        assert_eq!(
            classify_complexity(&clusters[0]),
            ComplexityClass::CrossCuttingRefactor
        );
    }

    #[test]
    fn classify_generator_level_for_generated_path() {
        let single = finding(
            "DR-001",
            FindingClass::Deepen,
            "Schema drift",
            "gen_schema",
            &["crates/foo/src/generated/schema.rs"],
            "single-row",
            "",
        );
        let cluster = &cluster_findings(&[single])[0];
        assert_eq!(classify_complexity(cluster), ComplexityClass::GeneratorLevel);
    }

    #[test]
    fn classify_generator_level_recognizes_build_rs() {
        let single = finding(
            "DR-001",
            FindingClass::Deepen,
            "Build script churn",
            "build_rs",
            &["crates/foo/build.rs"],
            "single-row",
            "",
        );
        let cluster = &cluster_findings(&[single])[0];
        assert_eq!(classify_complexity(cluster), ComplexityClass::GeneratorLevel);
    }

    #[test]
    fn classify_external_state_for_wall_clock_signal() {
        let single = finding(
            "DR-001",
            FindingClass::Deepen,
            "Pool acceptance lag",
            "pool",
            &["crates/pool/src/accept.rs"],
            "single-row",
            "needs wall-clock data captured from a live pool",
        );
        let cluster = &cluster_findings(&[single])[0];
        assert_eq!(classify_complexity(cluster), ComplexityClass::ExternalState);
    }

    #[test]
    fn classify_external_state_via_complexity_hint() {
        let single = finding(
            "DR-001",
            FindingClass::Deepen,
            "Settlement freshness",
            "settlement",
            &["crates/settlement/src/clock.rs"],
            "external-state",
            "",
        );
        let cluster = &cluster_findings(&[single])[0];
        assert_eq!(classify_complexity(cluster), ComplexityClass::ExternalState);
    }

    #[test]
    fn longest_common_ancestor_drops_trailing_filename() {
        let ancestor = longest_common_path_ancestor(&[
            "web/client/src/routes/a.rs".to_string(),
            "web/client/src/routes/b.rs".to_string(),
        ]);
        assert_eq!(ancestor, PathBuf::from("web/client/src/routes"));
    }

    #[test]
    fn longest_common_ancestor_empty_when_disjoint() {
        let ancestor = longest_common_path_ancestor(&[
            "src/a.rs".to_string(),
            "docs/b.md".to_string(),
        ]);
        assert_eq!(ancestor, PathBuf::new());
    }

    #[test]
    fn cluster_signature_separates_distinct_classes() {
        let findings = [
            finding(
                "DR-001",
                FindingClass::Simplify,
                "Tighten arm",
                "arm",
                &["src/util.rs"],
                "single-row",
                "",
            ),
            finding(
                "DR-002",
                FindingClass::Deepen,
                "Tighten arm",
                "arm",
                &["src/util.rs"],
                "single-row",
                "",
            ),
        ];
        let clusters = cluster_findings(&findings);
        assert_eq!(
            clusters.len(),
            2,
            "different classes must not collapse even with identical titles"
        );
    }
}
