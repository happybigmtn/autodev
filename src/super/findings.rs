// Typed schema for `super-findings.json` plus the deterministic resolver
// engine. Spliced into `super_command.rs` via `include!`; all imports and
// constants live in the parent module.

/// Canonical structure of `super-findings.json`. Single source of truth for the
/// CEO functional review pass; `SUPER-REPORT.md` is its rendered narrative view.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub(crate) struct SuperFindings {
    pub(crate) run_id: String,
    pub(crate) generated_at: String,
    pub(crate) readiness: String,
    #[serde(default)]
    pub(crate) blockers: Vec<SuperBlocker>,
    #[serde(default)]
    pub(crate) risks: Vec<SuperRisk>,
    #[serde(default)]
    pub(crate) gates: Vec<SuperGate>,
    pub(crate) campaign_plan: SuperCampaignPlan,
    /// Decisions parked for an operator. `policy = deterministic` entries with
    /// a `resolver_kind` are auto-resolved by `auto_resolve_deterministic_entries`
    /// before the execution gate; the rest stay here for human review.
    #[serde(default)]
    pub(crate) operator_queue: Vec<OperatorQueueEntry>,
    /// Audit trail of every entry resolved deterministically in-tree. Appended
    /// in resolution order; never replaces existing rows.
    #[serde(default)]
    pub(crate) auto_resolved: Vec<ResolvedEntry>,
}

/// Routing policy for an operator-queue entry.
///
/// - `Deterministic`: the entry's `resolver_kind` names an in-tree resolver
///   that can apply the change without a human or model call. Resolved
///   immediately at the end of corpus review.
/// - `External`: requires evidence from outside the repo (live runtime,
///   external service, third-party action). Parks for a human.
/// - `Manual`: technically deterministic but the operator has opted out of
///   auto-application (e.g. high-blast-radius config change).
#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum OperatorPolicy {
    Deterministic,
    External,
    Manual,
}

/// Tagged enum of in-tree resolvers. The variant data carries everything the
/// resolver needs; payloads stay in `OperatorQueueEntry.payload` only for the
/// human-readable rendering, not as input to the resolver itself.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub(crate) enum ResolverKind {
    /// Walk a baseline counter `current` toward `floor` by one step per call.
    BaselineWalk { current: u32, floor: u32 },
    /// Read a default value from `spec_path` (the first line that starts
    /// with `<var>=`); store the right-hand-side as the resolved value.
    EnvDefault { var: String, spec_path: PathBuf },
    /// Read a composition table from `RSOCIETY-GDD.md` under `section` and
    /// store the section body as the resolved value.
    GddComposition { section: String },
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub(crate) struct OperatorQueueEntry {
    pub(crate) id: String,
    pub(crate) title: String,
    pub(crate) policy: OperatorPolicy,
    #[serde(default)]
    pub(crate) resolver_kind: Option<ResolverKind>,
    #[serde(default)]
    pub(crate) payload: String,
    #[serde(default)]
    pub(crate) evidence: String,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub(crate) struct ResolvedEntry {
    pub(crate) entry_id: String,
    pub(crate) resolver: String,
    pub(crate) new_value: String,
    pub(crate) resolved_at: String,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub(crate) struct SuperBlocker {
    pub(crate) id: String,
    pub(crate) title: String,
    pub(crate) owner_surface: String,
    pub(crate) severity: String,
    pub(crate) evidence: String,
    pub(crate) remediation_hint: String,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub(crate) struct SuperRisk {
    pub(crate) id: String,
    pub(crate) title: String,
    pub(crate) likelihood: String,
    pub(crate) impact: String,
    pub(crate) mitigation: String,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub(crate) struct SuperGate {
    pub(crate) id: String,
    pub(crate) name: String,
    pub(crate) status: String,
    #[serde(default)]
    pub(crate) evidence_paths: Vec<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub(crate) struct SuperCampaignPlan {
    pub(crate) horizon_days: u32,
    #[serde(default)]
    pub(crate) milestones: Vec<SuperMilestone>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub(crate) struct SuperMilestone {
    pub(crate) id: String,
    pub(crate) title: String,
    pub(crate) day: u32,
    #[serde(default)]
    pub(crate) depends_on: Vec<String>,
}

/// Read `super-findings.json`, auto-resolve every deterministic operator-queue
/// entry in-place, and persist. Returns the number of entries resolved this
/// pass (0 when the queue is empty or only contains non-deterministic items).
fn auto_resolve_super_findings_in_place(repo_root: &Path, super_root: &Path) -> Result<usize> {
    let findings_path = super_root.join(SUPER_FINDINGS_FILE);
    if !findings_path.exists() {
        return Ok(0);
    }
    let text = fs::read_to_string(&findings_path)
        .with_context(|| format!("failed to read {}", findings_path.display()))?;
    let mut findings: SuperFindings = serde_json::from_str(&text)
        .with_context(|| format!("failed to parse {}", findings_path.display()))?;
    if findings.operator_queue.is_empty() {
        return Ok(0);
    }
    let resolved = auto_resolve_deterministic_entries(&mut findings, repo_root)?;
    let count = resolved.len();
    if count == 0 {
        return Ok(0);
    }
    atomic_write(&findings_path, &serde_json::to_vec_pretty(&findings)?)
        .with_context(|| format!("failed to write {}", findings_path.display()))?;
    Ok(count)
}

/// Apply each deterministic operator-queue entry's resolver. Drains resolved
/// entries from `findings.operator_queue` and appends a `ResolvedEntry` row
/// to `findings.auto_resolved`. Entries with `policy = external` or `manual`
/// are left in place, even if they carry a `resolver_kind`.
pub(crate) fn auto_resolve_deterministic_entries(
    findings: &mut SuperFindings,
    repo_root: &Path,
) -> Result<Vec<ResolvedEntry>> {
    let now = timestamp_slug();
    let mut resolved: Vec<ResolvedEntry> = Vec::new();
    let mut remaining: Vec<OperatorQueueEntry> = Vec::with_capacity(findings.operator_queue.len());

    let drained: Vec<OperatorQueueEntry> = std::mem::take(&mut findings.operator_queue);
    for entry in drained {
        if entry.policy != OperatorPolicy::Deterministic {
            remaining.push(entry);
            continue;
        }
        let Some(kind) = entry.resolver_kind.clone() else {
            remaining.push(entry);
            continue;
        };
        match apply_resolver(&kind, repo_root) {
            Ok(new_value) => {
                resolved.push(ResolvedEntry {
                    entry_id: entry.id.clone(),
                    resolver: resolver_label(&kind).to_string(),
                    new_value,
                    resolved_at: now.clone(),
                });
            }
            Err(err) => {
                // Resolver failed -- park the entry with a note in evidence
                // so the next operator pass sees why it could not resolve.
                let mut requeued = entry;
                let note = format!(
                    "\n[auto-resolve failed at {}: {}]",
                    now,
                    err.to_string().lines().next().unwrap_or("unknown")
                );
                requeued.evidence.push_str(&note);
                remaining.push(requeued);
            }
        }
    }

    findings.operator_queue = remaining;
    findings.auto_resolved.extend(resolved.iter().cloned());
    Ok(resolved)
}

fn resolver_label(kind: &ResolverKind) -> &'static str {
    match kind {
        ResolverKind::BaselineWalk { .. } => "baseline_walk",
        ResolverKind::EnvDefault { .. } => "env_default",
        ResolverKind::GddComposition { .. } => "gdd_composition",
    }
}

fn apply_resolver(kind: &ResolverKind, repo_root: &Path) -> Result<String> {
    match kind {
        ResolverKind::BaselineWalk { current, floor } => Ok(resolve_baseline_walk(*current, *floor)),
        ResolverKind::EnvDefault { var, spec_path } => {
            resolve_env_default(var, &absolutize_spec_path(repo_root, spec_path))
        }
        ResolverKind::GddComposition { section } => {
            let gdd = repo_root.join("RSOCIETY-GDD.md");
            resolve_gdd_composition(section, &gdd)
        }
    }
}

fn absolutize_spec_path(repo_root: &Path, spec_path: &Path) -> PathBuf {
    if spec_path.is_absolute() {
        spec_path.to_path_buf()
    } else {
        repo_root.join(spec_path)
    }
}

fn resolve_baseline_walk(current: u32, floor: u32) -> String {
    if current <= floor {
        return current.to_string();
    }
    let step = current.saturating_sub(floor).min(1);
    let next = current.saturating_sub(step);
    next.to_string()
}

fn resolve_env_default(var: &str, spec_path: &Path) -> Result<String> {
    let text = fs::read_to_string(spec_path)
        .with_context(|| format!("failed to read {}", spec_path.display()))?;
    let needle = format!("{var}=");
    for raw_line in text.lines() {
        let line = raw_line.trim_start_matches(|ch: char| ch == '-' || ch.is_whitespace());
        if let Some(rest) = line.strip_prefix(&needle) {
            let value = rest
                .trim_start()
                .trim_end_matches('`')
                .trim_start_matches('`')
                .trim();
            if !value.is_empty() {
                return Ok(value.to_string());
            }
        }
    }
    bail!(
        "no default for `{}` found in {}",
        var,
        spec_path.display()
    );
}

fn resolve_gdd_composition(section: &str, gdd_path: &Path) -> Result<String> {
    let text = fs::read_to_string(gdd_path)
        .with_context(|| format!("failed to read {}", gdd_path.display()))?;
    let mut capturing = false;
    let mut body: Vec<&str> = Vec::new();
    for line in text.lines() {
        let trimmed = line.trim_start();
        if trimmed.starts_with('#') {
            if capturing {
                break;
            }
            if line_matches_section_header(trimmed, section) {
                capturing = true;
                continue;
            }
        } else if capturing {
            body.push(line);
        }
    }
    if !capturing {
        bail!(
            "section `{}` not found in {}",
            section,
            gdd_path.display()
        );
    }
    let joined = body
        .iter()
        .map(|line| line.trim_end())
        .collect::<Vec<_>>()
        .join("\n");
    let trimmed = joined.trim().to_string();
    if trimmed.is_empty() {
        bail!(
            "section `{}` in {} is empty",
            section,
            gdd_path.display()
        );
    }
    Ok(trimmed)
}

fn line_matches_section_header(header_line: &str, section: &str) -> bool {
    let stripped = header_line.trim_start_matches('#').trim();
    stripped == section || stripped.eq_ignore_ascii_case(section)
}
