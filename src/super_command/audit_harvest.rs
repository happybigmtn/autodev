//! Audit-harvest pipeline: turn `auto audit --everything` findings into plan rows.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Stdio;

use anyhow::{bail, Context, Result};
use tokio::process::Command;

use crate::super_command::stages::run_super_codex_phase;
use crate::super_command::IMPLEMENTATION_PLAN;
use crate::util::{atomic_write, binary_provenance_line, git_repo_root};
use crate::{AuditHarvestArgs, SuperArgs};

/// Run `auto audit --everything` as a subprocess so the audit's clap defaults,
/// quota router, and per-file checkpointing all apply identically to a manual
/// invocation. Returns the run-id (either the supplied `--audit-run-id` or the
/// freshly generated one).
pub(crate) async fn run_super_audit_phase(args: &SuperArgs, repo_root: &Path) -> Result<String> {
    let audit_root = repo_root.join(".auto").join("audit-everything");
    fs::create_dir_all(&audit_root)
        .with_context(|| format!("failed to create {}", audit_root.display()))?;

    let auto_bin =
        std::env::current_exe().context("failed to resolve current `auto` binary path")?;
    let mut cmd = Command::new(&auto_bin);
    cmd.current_dir(repo_root)
        .arg("audit")
        .arg("--everything")
        .arg("--everything-threads")
        .arg(args.audit_threads.max(1).to_string())
        .arg("--remediation-threads")
        .arg(
            args.audit_threads
                .max(1)
                .saturating_div(2)
                .max(1)
                .to_string(),
        )
        .arg("--first-pass-retries")
        .arg(args.audit_first_pass_retries.to_string())
        .arg("--first-pass-model")
        .arg(&args.model)
        .arg("--first-pass-effort")
        .arg("low")
        .arg("--synthesis-model")
        .arg(&args.model)
        .arg("--synthesis-effort")
        .arg(&args.reasoning_effort)
        .arg("--codex-bin")
        .arg(&args.codex_bin);
    if let Some(run_id) = args.audit_run_id.as_deref() {
        cmd.arg("--everything-run-id").arg(run_id);
    }

    cmd.stdin(Stdio::null())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit());

    println!(
        "audit:       {} threads, {} retry round(s)",
        args.audit_threads.max(1),
        args.audit_first_pass_retries
    );
    let status = cmd
        .spawn()
        .with_context(|| format!("failed to spawn `{}` audit subprocess", auto_bin.display()))?
        .wait()
        .await
        .context("audit subprocess wait failed")?;
    if !status.success() {
        bail!("audit phase exited with status {status}");
    }

    if let Some(run_id) = args.audit_run_id.clone() {
        return Ok(run_id);
    }
    let latest_link = audit_root.join("latest-run");
    if latest_link.exists() {
        let target = fs::read_link(&latest_link)
            .or_else(|_| fs::read_to_string(&latest_link).map(PathBuf::from))
            .with_context(|| format!("failed to read {}", latest_link.display()))?;
        if let Some(name) = target.file_name().and_then(|s| s.to_str()) {
            return Ok(name.to_string());
        }
    }
    let mut latest: Option<(String, std::time::SystemTime)> = None;
    for entry in fs::read_dir(&audit_root)
        .with_context(|| format!("failed to read {}", audit_root.display()))?
    {
        let entry = entry?;
        if !entry.file_type()?.is_dir() {
            continue;
        }
        let name = entry.file_name().to_string_lossy().to_string();
        if name == "latest-run" {
            continue;
        }
        let mtime = entry
            .metadata()?
            .modified()
            .unwrap_or(std::time::UNIX_EPOCH);
        match &latest {
            None => latest = Some((name, mtime)),
            Some((_, t)) if mtime > *t => latest = Some((name, mtime)),
            _ => {}
        }
    }
    latest
        .map(|(name, _)| name)
        .context("audit completed but no run-id directory was found under .auto/audit-everything")
}

/// Harvest audit findings into `IMPLEMENTATION_PLAN.md` so the parallel stage
/// has actionable rows that target real audited files. Reads every
/// `analysis.json` under the audit run, ranks by score, and asks codex to
/// emit IMPLEMENTATION_PLAN.md task rows that follow the existing schema.
pub(crate) async fn run_super_audit_harvest(
    args: &SuperArgs,
    repo_root: &Path,
    super_root: &Path,
    audit_run_id: &str,
) -> Result<PathBuf> {
    harvest_audit_findings(
        repo_root,
        super_root,
        audit_run_id,
        &args.model,
        &args.reasoning_effort,
        &args.codex_bin,
        0,
        0,
        8,
    )
    .await
}

/// Standalone entrypoint for `auto audit-harvest --run-id <id>`. Resolves a
/// run-id (defaulting to the latest under `.auto/audit-everything/`) and
/// writes summary + IMPLEMENTATION_PLAN.md additions next to the audit run.
pub(crate) async fn run_audit_harvest_standalone(args: AuditHarvestArgs) -> Result<()> {
    let repo_root = git_repo_root()?;
    let audit_root = repo_root.join(".auto").join("audit-everything");
    let run_id = match args.run_id {
        Some(id) => id,
        None => resolve_latest_audit_run_id(&audit_root)?,
    };
    let harvest_root = audit_root.join(&run_id).join("harvest");
    fs::create_dir_all(&harvest_root)
        .with_context(|| format!("failed to create {}", harvest_root.display()))?;
    println!("audit-harvest");
    println!("binary:      {}", binary_provenance_line());
    println!("repo root:   {}", repo_root.display());
    println!("run-id:      {run_id}");
    println!("output:      {}", harvest_root.display());
    let max_findings = if args.max_findings == 0 {
        usize::MAX
    } else {
        args.max_findings
    };
    let summary = harvest_audit_findings(
        &repo_root,
        &harvest_root,
        &run_id,
        &args.model,
        &args.reasoning_effort,
        &args.codex_bin,
        max_findings,
        args.score_min,
        args.score_max,
    )
    .await?;
    println!("summary:     {}", summary.display());
    Ok(())
}

fn resolve_latest_audit_run_id(audit_root: &Path) -> Result<String> {
    let latest_link = audit_root.join("latest-run");
    if latest_link.exists() {
        let target = fs::read_link(&latest_link)
            .or_else(|_| fs::read_to_string(&latest_link).map(PathBuf::from))
            .with_context(|| format!("failed to read {}", latest_link.display()))?;
        if let Some(name) = target.file_name().and_then(|s| s.to_str()) {
            return Ok(name.to_string());
        }
    }
    let mut latest: Option<(String, std::time::SystemTime)> = None;
    for entry in fs::read_dir(audit_root)
        .with_context(|| format!("failed to read {}", audit_root.display()))?
    {
        let entry = entry?;
        if !entry.file_type()?.is_dir() {
            continue;
        }
        let name = entry.file_name().to_string_lossy().to_string();
        if name == "latest-run" {
            continue;
        }
        let mtime = entry
            .metadata()?
            .modified()
            .unwrap_or(std::time::UNIX_EPOCH);
        match &latest {
            None => latest = Some((name, mtime)),
            Some((_, t)) if mtime > *t => latest = Some((name, mtime)),
            _ => {}
        }
    }
    latest
        .map(|(name, _)| name)
        .context("no audit run-id directories found under .auto/audit-everything")
}

#[allow(clippy::too_many_arguments)]
async fn harvest_audit_findings(
    repo_root: &Path,
    output_root: &Path,
    audit_run_id: &str,
    model: &str,
    reasoning_effort: &str,
    codex_bin: &Path,
    max_findings: usize,
    score_min: i64,
    score_max: i64,
) -> Result<PathBuf> {
    let files_dir = repo_root
        .join(".auto")
        .join("audit-everything")
        .join(audit_run_id)
        .join("worktree")
        .join("audit")
        .join("everything")
        .join(audit_run_id)
        .join("files");
    if !files_dir.exists() {
        bail!(
            "audit harvest expected files dir at {} (run-id {audit_run_id})",
            files_dir.display(),
        );
    }

    // Build a registry of paths already covered by existing AUDIT-* rows
    // in IMPLEMENTATION_PLAN.md. Without this, the codex prompt's "skip
    // duplicates" directive is too lenient: phrasing variations across
    // iterations cause the same file to get harvested multiple times,
    // which is what made the 2026-05-08 iteration loop fail to converge
    // (plan grew from 62 → 90 [ ] over two iters).
    let plan_path = repo_root.join(IMPLEMENTATION_PLAN);
    let plan_existing_full = fs::read_to_string(&plan_path).unwrap_or_default();
    let already_covered_paths = collect_paths_from_audit_rows(&plan_existing_full);
    println!(
        "audit harvest: {} path(s) already covered by existing AUDIT-* rows; will dedup",
        already_covered_paths.len(),
    );

    let mut findings = Vec::new();
    let mut filtered_dup = 0usize;
    for entry in fs::read_dir(&files_dir)
        .with_context(|| format!("failed to read {}", files_dir.display()))?
    {
        let entry = entry?;
        if !entry.file_type()?.is_dir() {
            continue;
        }
        let analysis_json = entry.path().join("analysis.json");
        let Ok(text) = fs::read_to_string(&analysis_json) else {
            continue;
        };
        let Ok(value) = serde_json::from_str::<serde_json::Value>(&text) else {
            continue;
        };
        let score = value
            .get("score_out_of_10")
            .and_then(|v| v.as_i64())
            .unwrap_or(10);
        if score < score_min || score > score_max {
            continue;
        }
        let path = value.get("path").and_then(|v| v.as_str()).unwrap_or("");
        if !path.is_empty() && already_covered_paths.contains(path) {
            filtered_dup += 1;
            continue;
        }
        findings.push((score, value));
    }
    if filtered_dup > 0 {
        println!(
            "audit harvest: filtered {} finding(s) whose path is already in an existing AUDIT-* row",
            filtered_dup,
        );
    }
    findings.sort_by_key(|(score, _)| *score);
    let take = findings.len().min(max_findings);
    let actionable_full: Vec<&serde_json::Value> =
        findings.iter().take(take).map(|(_, v)| v).collect();

    let actionable_compact: Vec<serde_json::Value> = actionable_full
        .iter()
        .map(|v| compress_finding_for_harvest(v))
        .collect();

    fs::create_dir_all(output_root)
        .with_context(|| format!("failed to create {}", output_root.display()))?;
    let summary_path = output_root.join("AUDIT-FINDINGS-SUMMARY.json");
    atomic_write(
        &summary_path,
        &serde_json::to_vec_pretty(&serde_json::json!({
            "audit_run_id": audit_run_id,
            "score_min": score_min,
            "score_max": score_max,
            "matched_in_range": findings.len(),
            "harvested": actionable_compact.len(),
            "findings": actionable_compact,
        }))?,
    )
    .with_context(|| format!("failed to write {}", summary_path.display()))?;

    if actionable_compact.is_empty() {
        println!(
            "audit harvest: no findings in score range [{}..{}]; IMPLEMENTATION_PLAN.md unchanged",
            score_min, score_max,
        );
        return Ok(summary_path);
    }

    println!(
        "audit harvest: harvesting {} finding(s) from score range [{}..{}]",
        actionable_compact.len(),
        score_min,
        score_max,
    );
    let plan_path = repo_root.join(IMPLEMENTATION_PLAN);
    let chunks = chunk_findings_for_codex(&actionable_compact);
    let chunk_count = chunks.len();
    println!(
        "audit harvest: dispatching {} codex chunk(s) (codex hard-caps prompts at ~1MB)",
        chunk_count,
    );
    for (idx, chunk) in chunks.into_iter().enumerate() {
        let plan_existing = fs::read_to_string(&plan_path).unwrap_or_default();
        let phase_slug = if chunk_count == 1 {
            "audit-harvest".to_string()
        } else {
            format!("audit-harvest-chunk-{:02}-of-{:02}", idx + 1, chunk_count)
        };
        println!(
            "audit harvest: chunk {}/{} ({} findings)",
            idx + 1,
            chunk_count,
            chunk.len(),
        );
        let prompt =
            build_audit_harvest_prompt(&plan_existing, &chunk, audit_run_id, score_min, score_max);
        run_super_codex_phase(
            repo_root,
            output_root,
            &phase_slug,
            &prompt,
            model,
            reasoning_effort,
            codex_bin,
        )
        .await?;
    }
    println!(
        "audit harvest: appended task rows to {}",
        plan_path.display()
    );
    Ok(summary_path)
}

/// Codex's API gateway hard-caps prompt input at ~1 MB of UTF-8 characters.
/// Reserve ~80 KB for prompt boilerplate (instructions + plan excerpt) and
/// split the findings into chunks whose serialized JSON stays under the
/// remaining budget. Each chunk runs as its own codex call; the harvest
/// prompt's "scan existing IMPLEMENTATION_PLAN.md and skip duplicates"
/// directive prevents duplicate rows across chunks.
fn chunk_findings_for_codex(compressed: &[serde_json::Value]) -> Vec<Vec<serde_json::Value>> {
    const HARD_CAP_CHARS: usize = 1_000_000; // codex limit
    const PROMPT_OVERHEAD_CHARS: usize = 80_000; // boilerplate + plan excerpt
    const PER_CHUNK_BUDGET_CHARS: usize = HARD_CAP_CHARS - PROMPT_OVERHEAD_CHARS;

    let mut chunks: Vec<Vec<serde_json::Value>> = Vec::new();
    let mut current: Vec<serde_json::Value> = Vec::new();
    let mut current_chars: usize = 2; // for `[` `]`
    for finding in compressed {
        // serde_json::to_string never fails on owned Value; fall back to {} on
        // the impossible error path so we don't poison the chunk run.
        let serialized = serde_json::to_string(finding).unwrap_or_else(|_| "{}".to_string());
        let needed = serialized.chars().count() + 2; // entry + `,` separator
        if current_chars + needed > PER_CHUNK_BUDGET_CHARS && !current.is_empty() {
            chunks.push(std::mem::take(&mut current));
            current_chars = 2;
        }
        current.push(finding.clone());
        current_chars += needed;
    }
    if !current.is_empty() {
        chunks.push(current);
    }
    if chunks.is_empty() {
        chunks.push(Vec::new());
    }
    chunks
}

/// Compress an analysis.json down to the fields a harvest prompt actually
/// needs to write a task row. The verbose fields (`architecture_smells`,
/// Extract every file-like path token from existing AUDIT-* row blocks in
/// IMPLEMENTATION_PLAN.md. Paths in `Owns:`, `Source of truth:`, `Codebase
/// evidence:`, and `Spec:` lines all count. Used by harvest to dedup
/// findings whose target path is already covered by an existing row, even
/// if the new finding's wording differs from prior iterations' wording.
fn collect_paths_from_audit_rows(plan_text: &str) -> std::collections::HashSet<String> {
    use std::collections::HashSet;
    let mut paths: HashSet<String> = HashSet::new();
    let mut in_audit_block = false;
    let path_token = regex::Regex::new(
        r"`?\b((?:[A-Za-z0-9_./-]+/)?[A-Za-z0-9_-]+\.(?:rs|md|toml|json|sh|py|ts|tsx|js|jsx|yaml|yml|css|html|svg|txt|sql|move))\b`?",
    )
    .ok();
    for line in plan_text.lines() {
        let trimmed = line.trim_start();
        if trimmed.starts_with("- [")
            && (trimmed.contains("`AUDIT-") || trimmed.starts_with("- [ ] `AUDIT-"))
        {
            in_audit_block = true;
            continue;
        }
        if trimmed.starts_with("- [") {
            in_audit_block = false;
            continue;
        }
        if !in_audit_block {
            continue;
        }
        if let Some(re) = &path_token {
            for cap in re.captures_iter(line) {
                if let Some(m) = cap.get(1) {
                    paths.insert(m.as_str().to_string());
                }
            }
        }
    }
    paths
}

/// `behavior_preservation_needs`, `cross_file_questions`, etc.) get dropped
/// AND the surviving string values are truncated so thousands of findings
/// fit under the codex 1 MB prompt cap.
fn compress_finding_for_harvest(full: &serde_json::Value) -> serde_json::Value {
    fn truncate_string_value(v: &serde_json::Value, max: usize) -> serde_json::Value {
        match v {
            serde_json::Value::String(s) if s.chars().count() > max => {
                let mut shrunk: String = s.chars().take(max).collect();
                shrunk.push('…');
                serde_json::Value::String(shrunk)
            }
            other => other.clone(),
        }
    }
    let mut out = serde_json::Map::new();
    if let Some(v) = full.get("path") {
        out.insert("path".to_string(), v.clone());
    }
    if let Some(v) = full.get("group") {
        out.insert("group".to_string(), v.clone());
    }
    if let Some(v) = full.get("score_out_of_10") {
        out.insert("score_out_of_10".to_string(), v.clone());
    }
    if let Some(v) = full.get("summary") {
        out.insert("summary".to_string(), truncate_string_value(v, 240));
    }
    if let Some(arr) = full.get("recommended_actions").and_then(|v| v.as_array()) {
        let trimmed: Vec<serde_json::Value> = arr
            .iter()
            .take(2)
            .map(|v| truncate_string_value(v, 180))
            .collect();
        out.insert(
            "recommended_actions".to_string(),
            serde_json::Value::Array(trimmed),
        );
    }
    if let Some(arr) = full.get("ai_slop_signals").and_then(|v| v.as_array()) {
        let trimmed: Vec<serde_json::Value> = arr
            .iter()
            .take(2)
            .map(|v| truncate_string_value(v, 140))
            .collect();
        out.insert(
            "ai_slop_signals".to_string(),
            serde_json::Value::Array(trimmed),
        );
    }
    if let Some(arr) = full.get("deletion_candidates").and_then(|v| v.as_array()) {
        if let Some(first) = arr.first() {
            // Pull just `candidate` and `classification`; drop verbose evidence narratives.
            let mut compact = serde_json::Map::new();
            if let Some(c) = first.get("candidate") {
                compact.insert("candidate".to_string(), truncate_string_value(c, 160));
            }
            if let Some(c) = first.get("classification") {
                compact.insert("classification".to_string(), c.clone());
            }
            out.insert(
                "top_deletion_candidate".to_string(),
                serde_json::Value::Object(compact),
            );
        }
    }
    serde_json::Value::Object(out)
}

fn build_audit_harvest_prompt(
    plan_existing: &str,
    findings: &[serde_json::Value],
    audit_run_id: &str,
    score_min: i64,
    score_max: i64,
) -> String {
    let findings_json = serde_json::to_string_pretty(findings).unwrap_or_default();
    let plan_excerpt: String = plan_existing
        .lines()
        .take(60)
        .collect::<Vec<_>>()
        .join("\n");
    let cohort_label = if score_min == score_max {
        format!("score == {score_min}")
    } else {
        format!("scores {score_min}..={score_max}")
    };
    let consolidation_hint = if score_min >= 8 {
        "Many of these findings will share root causes (broad mild drift, repeated AI-slop patterns, schema gaps). Aggressively consolidate: one row per root cause, listing all affected paths in `Owns:` and `Integration touchpoints:`. A single thoughtful row that fixes 50 files is better than 50 thin rows."
    } else {
        "Each finding here is acute (low score). Prefer one task row per finding when the failure is file-specific, but still consolidate when several files share a clear root cause. Lean toward higher fidelity than for the score-8 cohort."
    };
    format!(
        "You are extending IMPLEMENTATION_PLAN.md with task rows that address findings from `auto audit --everything` run `{audit_run_id}`, restricted to {cohort_label}.

CONSTRAINTS:
- Append rows ONLY to the existing IMPLEMENTATION_PLAN.md. Do not edit other files. Do not create new files.
- Match the existing row schema exactly: every appended task block must use the `- [ ] `<ID>` <Title>` header followed by the indented field set seen in the existing rows (Spec / Why now / Codebase evidence / Source of truth / Runtime owner / UI consumers / Generated artifacts / Fixture boundary / Retired surfaces / Owns / Integration touchpoints / Scope boundary / Acceptance criteria / Verification / Required tests / Contract generation / Cross-surface tests / Review/closeout / Completion artifacts / Dependencies / Estimated scope / Completion signal).
- IDs must be unique across IMPLEMENTATION_PLAN.md. Use prefix `AUDIT-{audit_run_id}-NN`; scan the existing file first and start from the next free integer.
- {consolidation_hint}
- Skip duplicates of existing AUDIT-* rows. Skip findings whose `path` does not exist on disk.
- Acceptance criteria and Verification must reference real files and real cargo / pytest / shell commands the harness can run; no placeholders.
- Estimated scope must be XS, S, or M. Use M only when the row clearly spans multiple modules.

EXISTING IMPLEMENTATION_PLAN.md (first 60 lines for schema reference):
```
{plan_excerpt}
```

AUDIT FINDINGS (compressed JSON for {cohort_label}, ranked lowest-score first):
```json
{findings_json}
```

Now append the new task rows to IMPLEMENTATION_PLAN.md. Do not modify existing rows. Verify the file parses by re-reading it after the append. Report a one-line summary of how many rows you appended and the ID range used.
"
    )
}
