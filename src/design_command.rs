use std::collections::BTreeSet;
use std::fs;
use std::path::Path;

use anyhow::{bail, Context, Result};
use regex::Regex;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::backend_policy::{PipelineStage, PromptTier};
use crate::codex_exec::run_codex_exec_max_context;
use crate::parallel_command;
use crate::prompt_builder::{EthosPosture, PromptSpec};
use crate::qa_only_command::{
    allowed_report_only_dirty_paths, collect_dirty_state, print_final_status_block,
    report_only_dirty_state_report,
};
use crate::task_parser::{parse_tasks, TaskStatus};
use crate::util::{
    atomic_write, auto_checkpoint_if_needed, binary_provenance_line, ensure_repo_layout,
    git_repo_root, git_stdout, timestamp_slug,
};
use crate::verdict::{exact_terminal_verdict, terminal_verdict_is};
use crate::{DesignArgs, ParallelAction, ParallelArgs, ParallelCargoTarget, SuperArgs};

/// Env override allowing operators to disable the stall short-circuit. When set
/// (any non-empty value), `auto design --resolve` continues even after two
/// consecutive passes produce identical blocker fingerprints. The default is to
/// abort early so the budget is not burned on identical no-progress retries.
const FORCE_STALLED_CONTINUE_ENV: &str = "AUTODEV_DESIGN_GATE_FORCE_STALLED_CONTINUE";

/// Stable kind tag the LLM emits for blockers that re-running the design gate
/// cannot fix (operated readback drift, strict-audit red, browser proof red,
/// fixture freshness, etc.). When *every* blocker in a pass carries this kind
/// the gate short-circuits with a dedicated error rather than retrying.
const BLOCKER_KIND_EXTERNAL_RUNTIME_DATA: &str = "external-runtime-data";

/// Stable kind tag for findings that another design pass plausibly fixes
/// (doctrine, design tokens, IA, copy, accessibility, runtime/UI contract
/// authoring). At least one design-quality blocker keeps the retry loop alive.
/// Held as a const for parity with `BLOCKER_KIND_EXTERNAL_RUNTIME_DATA` and so
/// future routing code (kind-aware retry policies, telemetry) shares one
/// spelling; the literal also appears in the prompt and tests.
#[allow(dead_code)]
const BLOCKER_KIND_DESIGN_QUALITY: &str = "design-quality";

const DESIGN_ARTIFACTS: [&str; 6] = [
    "DESIGN-AUDIT.md",
    "DESIGN-SYSTEM-PROPOSAL.md",
    "ENGINE-UI-CONTRACT.md",
    "FRONTEND-QA.md",
    "DESIGN-PLAN-ITEMS.md",
    "DESIGN-REPORT.md",
];

/// Stable digest of a NO-GO pass's blocker surface. Two passes with the same
/// fingerprint produced the same list of blocker IDs and the same set of
/// `Required blockers before merge` stems. That signals zero forward progress
/// and is the trigger for the stall short-circuit.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
struct BlockerFingerprint {
    /// SHA-256 hex digest (first 16 chars) of the canonicalized blocker set.
    /// `"unstructured"` when the report has no parsable blocker list -- the
    /// LLM either produced free prose or did not emit the canonical section.
    digest: String,
    /// Sorted, deduplicated blocker IDs extracted from the report (e.g.
    /// `DESIGN-120526-01`). Empty when the report had no recognizable rows.
    blocker_ids: Vec<String>,
    /// Sorted set of `kind:` annotations seen in the report (e.g.
    /// `design-quality`, `external-runtime-data`). Empty when no kinds were
    /// emitted -- treated as legacy "unknown kind" for short-circuit purposes.
    kinds: Vec<String>,
    /// Total parsed blocker count (blocker_ids.len() before dedup). Useful for
    /// the operator status line and for spotting reports that lost blockers
    /// between passes.
    blocker_count: usize,
}

/// One entry in the persisted `progress.json` history. The file is appended to
/// after every pass so the next pass can compare its fingerprint against the
/// previous one without re-parsing every earlier `DESIGN-REPORT.md`.
#[derive(Clone, Debug, Serialize, Deserialize)]
struct PassProgress {
    pass: usize,
    verdict: String,
    fingerprint: String,
    blocker_count: usize,
    /// True when every parsed blocker carried `kind: external-runtime-data`
    /// (or an equivalent stable alias). Triggers the dedicated "design cannot
    /// affect this" short-circuit instead of the generic stall error.
    all_external_runtime_data: bool,
    /// ISO 8601 UTC timestamp the pass landed.
    completed_at: String,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
struct ProgressLog {
    passes: Vec<PassProgress>,
}

impl ProgressLog {
    fn load_or_default(path: &Path) -> Result<Self> {
        if !path.exists() {
            return Ok(Self::default());
        }
        let raw = fs::read_to_string(path)
            .with_context(|| format!("failed to read {}", path.display()))?;
        let log: Self = serde_json::from_str(&raw)
            .with_context(|| format!("failed to parse {}", path.display()))?;
        Ok(log)
    }

    fn write(&self, path: &Path) -> Result<()> {
        let bytes = serde_json::to_vec_pretty(self)
            .context("failed to serialize design progress log")?;
        atomic_write(path, &bytes)
            .with_context(|| format!("failed to write {}", path.display()))?;
        Ok(())
    }
}

/// Outcome of comparing a fresh NO-GO pass against the prior history.
enum ProgressOutcome {
    /// Fingerprint differs from prior pass (or there is no prior pass).
    /// Continue the loop.
    Progressed,
    /// Pass N fingerprint matched pass N-1 fingerprint. Caller should abort
    /// unless `AUTODEV_DESIGN_GATE_FORCE_STALLED_CONTINUE` is set.
    Stalled { prior_pass: usize },
    /// Every blocker in the current pass is `external-runtime-data` (or an
    /// equivalent kind). Caller should abort with a short-circuit error
    /// because design re-runs cannot move repo-external state.
    AllExternalRuntimeData,
}

/// Parse a `DESIGN-REPORT.md` body and return its blocker fingerprint. Pulls
/// from two surfaces:
///   1. Inline `- [ ] DESIGN-XXX ...` and `- [!] DESIGN-XXX ...` rows anywhere
///      in the document (the `[ ]`/`[!]` checkbox marks the LLM uses for
///      open work and explicit blockers).
///   2. A canonical `## Required Blockers Before Merge` section when present:
///      each row of that section is reduced to a stable stem (date stamps and
///      hashes stripped) and folded into the digest.
/// `kind:` annotations of the form `kind: external-runtime-data` (or
/// `kind=external-runtime-data`) are extracted into the `kinds` set.
fn fingerprint_design_report(report: &str) -> BlockerFingerprint {
    let id_re = Regex::new(r"(?m)^\s*-\s*\[(?:[ !~xX])\]\s*`?(DESIGN-[A-Z0-9_-]+)")
        .expect("static regex");
    let mut ids: BTreeSet<String> = BTreeSet::new();
    for capture in id_re.captures_iter(report) {
        if let Some(matched) = capture.get(1) {
            ids.insert(matched.as_str().to_string());
        }
    }

    let blocker_section_stems = extract_required_blocker_stems(report);

    let kind_re = Regex::new(r"(?i)kind\s*[:=]\s*([a-z][a-z0-9_.-]+)").expect("static regex");
    let mut kinds: BTreeSet<String> = BTreeSet::new();
    for capture in kind_re.captures_iter(report) {
        if let Some(matched) = capture.get(1) {
            kinds.insert(matched.as_str().to_ascii_lowercase());
        }
    }

    let blocker_ids: Vec<String> = ids.into_iter().collect();
    let kinds_vec: Vec<String> = kinds.into_iter().collect();

    if blocker_ids.is_empty() && blocker_section_stems.is_empty() && kinds_vec.is_empty() {
        return BlockerFingerprint {
            digest: "unstructured".to_string(),
            blocker_ids,
            kinds: kinds_vec,
            blocker_count: 0,
        };
    }

    let mut hasher = Sha256::new();
    hasher.update(b"design-blocker-fingerprint-v1\n");
    for id in &blocker_ids {
        hasher.update(b"id:");
        hasher.update(id.as_bytes());
        hasher.update(b"\n");
    }
    for stem in &blocker_section_stems {
        hasher.update(b"stem:");
        hasher.update(stem.as_bytes());
        hasher.update(b"\n");
    }
    for kind in &kinds_vec {
        hasher.update(b"kind:");
        hasher.update(kind.as_bytes());
        hasher.update(b"\n");
    }
    let full = hex_lower(&hasher.finalize());
    let digest = full[..16].to_string();
    BlockerFingerprint {
        digest,
        blocker_count: blocker_ids.len().max(blocker_section_stems.len()),
        blocker_ids,
        kinds: kinds_vec,
    }
}

fn hex_lower(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push_str(&format!("{byte:02x}"));
    }
    out
}

/// Lift the `## Required Blockers Before Merge` section (if any) into stable
/// stems. Each line is lower-cased, stripped of leading list markers, ISO
/// dates (`20260512`, `2026-05-12`, `120526`), and inline hex hashes so two
/// passes describing the same blocker in slightly different words still fold
/// to the same fingerprint.
fn extract_required_blocker_stems(report: &str) -> Vec<String> {
    let header_re = Regex::new(r"(?im)^\s*#{1,6}\s*required\s+blockers?\s+before\s+merge")
        .expect("static regex");
    let Some(header_match) = header_re.find(report) else {
        return Vec::new();
    };
    let body = &report[header_match.end()..];
    let mut stems: BTreeSet<String> = BTreeSet::new();
    for line in body.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        if trimmed.starts_with('#') {
            break;
        }
        // Only list rows count as blockers. Free prose lines (e.g. trailing
        // `Verdict: NO-GO` or commentary) must not hash into the fingerprint.
        if !trimmed.starts_with('-') && !trimmed.starts_with('*') {
            continue;
        }
        let cleaned = canonicalize_blocker_stem(trimmed);
        if !cleaned.is_empty() {
            stems.insert(cleaned);
        }
    }
    stems.into_iter().collect()
}

/// Reduce a free-form blocker line to a stable stem suitable for hashing.
fn canonicalize_blocker_stem(line: &str) -> String {
    let mut value = line.to_string();
    // Drop checkbox prefixes (`- [ ]`, `- [!]`, `- [~]`, `- [x]`) and bullets.
    let prefix_re = Regex::new(r"^[\-\*]\s*(\[[ xX!~]\]\s*)?").expect("static regex");
    value = prefix_re.replace(&value, "").to_string();
    // Strip ISO 8601 dates and YYMMDD-style stamps the LLM threads into IDs.
    let date_re =
        Regex::new(r"\b(20\d{2}[-_]?\d{2}[-_]?\d{2}|\d{6}-\d{2})\b").expect("static regex");
    value = date_re.replace_all(&value, "").to_string();
    // Strip 8+-char hex hashes (e.g. commit fragments) that drift pass to pass.
    let hash_re = Regex::new(r"\b[0-9a-f]{8,}\b").expect("static regex");
    value = hash_re.replace_all(&value, "").to_string();
    // Strip parenthetical asides (`(date 2026-05-13)`, `(commit abcd1234)`,
    // `(see PR #42)`). These are commentary that drifts pass-to-pass even when
    // the underlying blocker is identical. The core stem -- ID + kind +
    // summary -- is what we want to hash.
    let paren_re = Regex::new(r"\s*\([^()]*\)").expect("static regex");
    value = paren_re.replace_all(&value, "").to_string();
    // Collapse whitespace and drop trailing punctuation noise.
    let ws_re = Regex::new(r"\s+").expect("static regex");
    value = ws_re.replace_all(value.trim(), " ").to_string();
    value = value
        .trim_matches(|c: char| c == '.' || c == ',' || c == ';')
        .to_string();
    value.to_ascii_lowercase()
}

/// True when every parsed kind annotation is `external-runtime-data` (or an
/// equivalent stable alias) and at least one kind was present. Returns false
/// when the report did not annotate any kinds -- we cannot infer external
/// status from absence.
fn all_blockers_external_runtime_data(fingerprint: &BlockerFingerprint) -> bool {
    if fingerprint.kinds.is_empty() {
        return false;
    }
    fingerprint
        .kinds
        .iter()
        .all(|kind| kind == BLOCKER_KIND_EXTERNAL_RUNTIME_DATA)
}

fn record_pass_progress(
    progress_path: &Path,
    pass: usize,
    verdict: &str,
    fingerprint: &BlockerFingerprint,
) -> Result<ProgressOutcome> {
    let mut log = ProgressLog::load_or_default(progress_path)?;
    let prior = log.passes.last().cloned();
    let entry = PassProgress {
        pass,
        verdict: verdict.to_string(),
        fingerprint: fingerprint.digest.clone(),
        blocker_count: fingerprint.blocker_count,
        all_external_runtime_data: all_blockers_external_runtime_data(fingerprint),
        completed_at: chrono::Utc::now().to_rfc3339(),
    };
    log.passes.push(entry.clone());
    log.write(progress_path)?;

    if entry.all_external_runtime_data {
        return Ok(ProgressOutcome::AllExternalRuntimeData);
    }
    if let Some(prior) = prior {
        if prior.fingerprint == entry.fingerprint && entry.fingerprint != "unstructured" {
            return Ok(ProgressOutcome::Stalled {
                prior_pass: prior.pass,
            });
        }
    }
    Ok(ProgressOutcome::Progressed)
}

fn stalled_short_circuit_enabled() -> bool {
    std::env::var(FORCE_STALLED_CONTINUE_ENV)
        .map(|value| value.trim().is_empty())
        .unwrap_or(true)
}

#[derive(Serialize)]
struct DesignManifest {
    run_id: String,
    repo_root: String,
    planning_root: Option<String>,
    output_dir: String,
    prompt: Option<String>,
    model: String,
    reasoning_effort: String,
    apply: bool,
    resolve: bool,
    resolve_passes: usize,
    skip_qa: bool,
    binary: String,
}

pub(crate) async fn run_design(args: DesignArgs) -> Result<()> {
    let repo_root = git_repo_root()?;
    ensure_repo_layout(&repo_root)?;
    if args.resolve {
        return run_design_resolution(args, DesignRunKind::Resolve).await;
    }

    let run_id = timestamp_slug();
    let output_dir = args
        .output_dir
        .clone()
        .unwrap_or_else(|| repo_root.join(".auto").join("design").join(&run_id));
    let planning_root = args.planning_root.clone().or_else(|| {
        repo_root
            .join("genesis")
            .exists()
            .then(|| repo_root.join("genesis"))
    });

    fs::create_dir_all(&output_dir)
        .with_context(|| format!("failed to create {}", output_dir.display()))?;

    let manifest = DesignManifest {
        run_id,
        repo_root: repo_root.display().to_string(),
        planning_root: planning_root
            .as_ref()
            .map(|path| path.display().to_string()),
        output_dir: output_dir.display().to_string(),
        prompt: args.prompt.clone(),
        model: args.model.clone(),
        reasoning_effort: args.reasoning_effort.clone(),
        apply: args.apply,
        resolve: false,
        resolve_passes: 1,
        skip_qa: args.skip_qa,
        binary: binary_provenance_line(),
    };
    atomic_write(
        &output_dir.join("manifest.json"),
        &serde_json::to_vec_pretty(&manifest)?,
    )
    .with_context(|| {
        format!(
            "failed to write {}",
            output_dir.join("manifest.json").display()
        )
    })?;

    let prompt = build_design_prompt(
        &repo_root,
        planning_root.as_deref(),
        &output_dir,
        args.prompt.as_deref(),
        args.apply,
        args.skip_qa,
        DesignRunKind::Standalone,
    );
    let prompt_path = output_dir.join("design-prompt.md");
    atomic_write(&prompt_path, prompt.as_bytes())
        .with_context(|| format!("failed to write {}", prompt_path.display()))?;

    println!("auto design");
    println!("binary:      {}", binary_provenance_line());
    println!("repo root:   {}", repo_root.display());
    if let Some(planning_root) = &planning_root {
        println!("planning:    {}", planning_root.display());
    }
    println!("output dir:  {}", output_dir.display());
    println!("model:       {}", args.model);
    println!("effort:      {}", args.reasoning_effort);
    println!("apply:       {}", if args.apply { "yes" } else { "no" });
    println!(
        "qa:          {}",
        if args.skip_qa { "skipped" } else { "enabled" }
    );
    println!("prompt log:  {}", prompt_path.display());

    if args.dry_run {
        println!("\n{prompt}");
        print_final_status_block(
            "design dry-run prompt rendered",
            &[
                output_dir.join("manifest.json").display().to_string(),
                prompt_path.display().to_string(),
            ],
            "design worker not invoked",
            "run auto design without --dry-run to produce DESIGN-REPORT.md",
        );
        return Ok(());
    }

    let report_only_baseline = if args.apply {
        None
    } else {
        Some(collect_dirty_state(&repo_root)?)
    };
    let phase_result = run_design_codex_phase(
        &repo_root,
        &output_dir,
        &prompt,
        &args.model,
        &args.reasoning_effort,
        &args.codex_bin,
        "auto-design",
    )
    .await;
    if let Some(baseline) = &report_only_baseline {
        enforce_design_report_only_write_boundary(&repo_root, &output_dir, baseline)?;
    }
    phase_result?;
    verify_design_artifacts(&output_dir)?;
    println!("status:      design artifacts verified");
    print_final_status_block(
        "design artifacts verified",
        &DESIGN_ARTIFACTS
            .iter()
            .map(|artifact| output_dir.join(artifact).display().to_string())
            .chain([
                output_dir.join("manifest.json").display().to_string(),
                prompt_path.display().to_string(),
                output_dir
                    .join("auto-design-stderr.log")
                    .display()
                    .to_string(),
            ])
            .collect::<Vec<_>>(),
        "none",
        "review DESIGN-REPORT.md verdict before running auto gen, auto parallel, or auto design --resolve",
    );
    Ok(())
}

async fn run_design_resolution(args: DesignArgs, kind: DesignRunKind) -> Result<()> {
    let repo_root = git_repo_root()?;
    ensure_repo_layout(&repo_root)?;
    let run_id = timestamp_slug();
    let output_root = args
        .output_dir
        .clone()
        .unwrap_or_else(|| repo_root.join(".auto").join("design").join(&run_id));
    let planning_root = args.planning_root.clone().or_else(|| {
        repo_root
            .join("genesis")
            .exists()
            .then(|| repo_root.join("genesis"))
    });
    fs::create_dir_all(&output_root)
        .with_context(|| format!("failed to create {}", output_root.display()))?;

    let max_passes = args.resolve_passes.max(1);
    let manifest = DesignManifest {
        run_id,
        repo_root: repo_root.display().to_string(),
        planning_root: planning_root
            .as_ref()
            .map(|path| path.display().to_string()),
        output_dir: output_root.display().to_string(),
        prompt: args.prompt.clone(),
        model: args.model.clone(),
        reasoning_effort: args.reasoning_effort.clone(),
        apply: true,
        resolve: true,
        resolve_passes: max_passes,
        skip_qa: args.skip_qa,
        binary: binary_provenance_line(),
    };
    atomic_write(
        &output_root.join("manifest.json"),
        &serde_json::to_vec_pretty(&manifest)?,
    )
    .with_context(|| {
        format!(
            "failed to write {}",
            output_root.join("manifest.json").display()
        )
    })?;

    println!("auto design --resolve");
    println!("binary:      {}", binary_provenance_line());
    println!("repo root:   {}", repo_root.display());
    if let Some(planning_root) = &planning_root {
        println!("planning:    {}", planning_root.display());
    }
    println!("output root: {}", output_root.display());
    println!("model:       {}", args.model);
    println!("effort:      {}", args.reasoning_effort);
    println!("passes:      {max_passes}");
    println!("workers:     {}", args.max_concurrent_workers.max(1));
    println!(
        "qa:          {}",
        if args.skip_qa { "skipped" } else { "enabled" }
    );

    if args.dry_run {
        let prompt = build_design_prompt(
            &repo_root,
            planning_root.as_deref(),
            &output_root.join("pass-01"),
            args.prompt.as_deref(),
            true,
            args.skip_qa,
            kind,
        );
        println!("\n{prompt}");
        print_final_status_block(
            "design resolve dry-run prompt rendered",
            &[output_root.join("manifest.json").display().to_string()],
            "design worker not invoked",
            "run auto design --resolve without --dry-run to produce DESIGN-REPORT.md",
        );
        return Ok(());
    }

    let mut last_report = None;
    let mut pass = 1usize;
    let mut recovery_extensions = 0usize;
    let max_recovery_extensions = match kind {
        DesignRunKind::SuperResolve => max_passes,
        _ => 0,
    };
    while pass <= max_passes + max_recovery_extensions {
        let pass_dir = output_root.join(format!("pass-{pass:02}"));
        fs::create_dir_all(&pass_dir)
            .with_context(|| format!("failed to create {}", pass_dir.display()))?;
        println!("stage:       design resolve pass {pass}/{max_passes}");
        let prompt = build_design_prompt(
            &repo_root,
            planning_root.as_deref(),
            &pass_dir,
            args.prompt.as_deref(),
            true,
            args.skip_qa,
            kind,
        );
        let prompt_path = pass_dir.join("design-prompt.md");
        atomic_write(&prompt_path, prompt.as_bytes())
            .with_context(|| format!("failed to write {}", prompt_path.display()))?;
        run_design_codex_phase(
            &repo_root,
            &pass_dir,
            &prompt,
            &args.model,
            &args.reasoning_effort,
            &args.codex_bin,
            &format!("auto-design-resolve-pass-{pass:02}"),
        )
        .await?;
        verify_design_artifacts(&pass_dir)?;
        last_report = Some(pass_dir.join("DESIGN-REPORT.md"));
        write_design_resolution_status(&output_root, pass, max_passes, &pass_dir, "audited")?;
        if design_report_is_go(&pass_dir)? {
            write_design_resolution_status(&output_root, pass, max_passes, &pass_dir, "verified")?;
            println!("status:      design resolve verified");
            println!("pass dir:    {}", pass_dir.display());
            print_final_status_block(
                "design resolve verified",
                &[
                    pass_dir.join("DESIGN-REPORT.md").display().to_string(),
                    output_root
                        .join("DESIGN-RESOLVE-STATUS.md")
                        .display()
                        .to_string(),
                ],
                "none",
                "continue the production campaign or run auto gen with the promoted design contract",
            );
            return Ok(());
        }

        // NO-GO branch: fingerprint the blockers and record into progress.json.
        // If pass N produced the same blockers as pass N-1, abort early -- we
        // are about to spend another model budget on a verbatim retry.
        let report_text = fs::read_to_string(pass_dir.join("DESIGN-REPORT.md")).with_context(
            || {
                format!(
                    "failed to read {}",
                    pass_dir.join("DESIGN-REPORT.md").display()
                )
            },
        )?;
        let fingerprint = fingerprint_design_report(&report_text);
        println!(
            "fingerprint: pass {pass} blocker_digest={} blocker_count={} kinds={}",
            fingerprint.digest,
            fingerprint.blocker_count,
            if fingerprint.kinds.is_empty() {
                "<none>".to_string()
            } else {
                fingerprint.kinds.join(",")
            }
        );
        let outcome = record_pass_progress(
            &output_root.join("progress.json"),
            pass,
            "NO-GO",
            &fingerprint,
        )?;
        match outcome {
            ProgressOutcome::Progressed => {}
            ProgressOutcome::Stalled { prior_pass } => {
                if stalled_short_circuit_enabled() {
                    write_design_resolution_status(
                        &output_root,
                        pass,
                        max_passes,
                        &pass_dir,
                        "stalled-no-progress",
                    )?;
                    if kind == DesignRunKind::SuperResolve {
                        try_checkpoint_final_design_resolve_state(
                            &repo_root,
                            args.branch.as_deref(),
                        );
                    }
                    bail!(
                        "design resolve stalled at pass {pass}: blocker fingerprint unchanged from pass {prior_pass} (no progress); \
                         set {FORCE_STALLED_CONTINUE_ENV}=1 to override and keep retrying, or run an implementation pass against the carried `DESIGN-PLAN-ITEMS.md`"
                    );
                } else {
                    eprintln!(
                        "warning: design resolve fingerprint unchanged from pass {prior_pass}; \
                         {FORCE_STALLED_CONTINUE_ENV} override set, continuing"
                    );
                }
            }
            ProgressOutcome::AllExternalRuntimeData => {
                write_design_resolution_status(
                    &output_root,
                    pass,
                    max_passes,
                    &pass_dir,
                    "external-runtime-data-only",
                )?;
                if kind == DesignRunKind::SuperResolve {
                    try_checkpoint_final_design_resolve_state(&repo_root, args.branch.as_deref());
                }
                bail!(
                    "design resolve cannot proceed: every blocker at pass {pass} is `kind: {BLOCKER_KIND_EXTERNAL_RUNTIME_DATA}` (repo-external state design re-runs cannot affect); \
                     re-run after the runtime/data dependency clears, or rerun the upstream gate with `--skip-design` and operator review"
                );
            }
        }

        if pass >= max_passes {
            let promoted = preserve_final_no_go_design_plan_items(
                &repo_root,
                &output_root,
                pass,
                max_passes,
                &pass_dir,
            )?;
            if let Some(promoted) = promoted {
                println!(
                    "status:      promoted {promoted} design task(s) into IMPLEMENTATION_PLAN.md"
                );
            }
            if recovery_extensions < max_recovery_extensions
                && root_queue_has_dependency_ready_repair_tasks(&repo_root)?
            {
                recovery_extensions += 1;
                println!(
                    "stage:       final NO-GO repair implementation {recovery_extensions}/{max_recovery_extensions}"
                );
                run_design_parallel_pass(&args, &output_root, pass).await?;
                write_design_resolution_status(
                    &output_root,
                    pass,
                    max_passes,
                    &pass_dir,
                    "final-no-go-repair-pass-complete",
                )?;
                pass += 1;
                continue;
            }
            break;
        }
        if let Some(promoted) = promote_design_plan_items_to_root_queue(&repo_root, &pass_dir)? {
            println!("status:      promoted {promoted} design task(s) into IMPLEMENTATION_PLAN.md");
        }
        println!("stage:       design implementation pass {pass}/{max_passes}");
        run_design_parallel_pass(&args, &output_root, pass).await?;
        write_design_resolution_status(
            &output_root,
            pass,
            max_passes,
            &pass_dir,
            "implementation-pass-complete",
        )?;
        pass += 1;
    }

    let report = last_report
        .map(|path| path.display().to_string())
        .unwrap_or_else(|| output_root.display().to_string());
    if kind == DesignRunKind::SuperResolve {
        try_checkpoint_final_design_resolve_state(&repo_root, args.branch.as_deref());
    }
    bail!("design resolve did not reach `Verdict: GO` after {max_passes} pass(es); latest report: {report}")
}

fn try_checkpoint_final_design_resolve_state(repo_root: &Path, branch: Option<&str>) {
    let target_branch = branch
        .map(str::to_string)
        .or_else(|| {
            git_stdout(repo_root, ["branch", "--show-current"])
                .ok()
                .map(|branch| branch.trim().to_string())
        })
        .unwrap_or_default();
    if target_branch.is_empty() {
        eprintln!(
            "warning: design resolve ended NO-GO with possible repo edits, but no checked-out branch was available for checkpointing"
        );
        return;
    }
    match auto_checkpoint_if_needed(repo_root, &target_branch, "design resolve NO-GO checkpoint") {
        Ok(Some(commit)) => eprintln!(
            "checkpoint: committed final design resolve state at {commit} before reporting NO-GO"
        ),
        Ok(None) => {}
        Err(err) => eprintln!(
            "warning: failed to checkpoint final design resolve state before reporting NO-GO: {err:#}"
        ),
    }
}

fn root_queue_has_dependency_ready_repair_tasks(repo_root: &Path) -> Result<bool> {
    let plan_path = repo_root.join("IMPLEMENTATION_PLAN.md");
    if !plan_path.exists() {
        return Ok(false);
    }
    let plan = fs::read_to_string(&plan_path)
        .with_context(|| format!("failed to read {}", plan_path.display()))?;
    let tasks = parse_tasks(&plan);
    let completed = tasks
        .iter()
        .filter(|task| task.status == TaskStatus::Done)
        .map(|task| task.id.as_str())
        .collect::<std::collections::BTreeSet<_>>();
    Ok(tasks.iter().any(|task| {
        matches!(task.status, TaskStatus::Pending | TaskStatus::Partial)
            && task
                .dependencies
                .iter()
                .all(|dependency| completed.contains(dependency.as_str()))
    }))
}

fn enforce_design_report_only_write_boundary(
    repo_root: &Path,
    output_dir: &Path,
    baseline: &[crate::qa_only_command::DirtyEntry],
) -> Result<()> {
    let allowed_paths =
        allowed_report_only_dirty_paths(repo_root, output_dir, ".auto/design", ".auto/design");
    let dirty_report = report_only_dirty_state_report(repo_root, baseline, &allowed_paths)?;
    if dirty_report.has_violations() {
        bail!(
            "{}",
            dirty_report.render("auto design", "the design output directory")
        );
    }
    if dirty_report.has_preexisting_dirty_state() {
        eprintln!("{}", dirty_report.render_preexisting());
    }
    Ok(())
}

async fn run_design_parallel_pass(
    args: &DesignArgs,
    output_root: &Path,
    pass: usize,
) -> Result<()> {
    parallel_command::run_parallel_inline(ParallelArgs {
        action: None::<ParallelAction>,
        max_iterations: args.max_iterations,
        max_concurrent_workers: args.max_concurrent_workers.max(1),
        cargo_build_jobs: None,
        cargo_target: ParallelCargoTarget::Auto,
        prompt_file: None,
        model: args.worker_model.clone(),
        reasoning_effort: args.worker_reasoning_effort.clone(),
        branch: args.branch.clone(),
        reference_repos: args.reference_repos.clone(),
        include_siblings: false,
        run_root: Some(output_root.join("parallel").join(format!("pass-{pass:02}"))),
        codex_bin: args.codex_bin.clone(),
        claude: false,
        max_turns: None,
        max_retries: 2,
    })
    .await
}

fn write_design_resolution_status(
    output_root: &Path,
    pass: usize,
    max_passes: usize,
    pass_dir: &Path,
    status: &str,
) -> Result<()> {
    let markdown = format!(
        "# Design Resolve Status\n\n- Status: `{status}`\n- Pass: `{pass}/{max_passes}`\n- Latest artifacts: `{}`\n- Latest report: `{}`\n",
        pass_dir.display(),
        pass_dir.join("DESIGN-REPORT.md").display()
    );
    atomic_write(
        &output_root.join("DESIGN-RESOLVE-STATUS.md"),
        markdown.as_bytes(),
    )
    .with_context(|| {
        format!(
            "failed to write {}",
            output_root.join("DESIGN-RESOLVE-STATUS.md").display()
        )
    })
}

fn preserve_final_no_go_design_plan_items(
    repo_root: &Path,
    output_root: &Path,
    pass: usize,
    max_passes: usize,
    pass_dir: &Path,
) -> Result<Option<usize>> {
    let promoted = promote_design_plan_items_to_root_queue(repo_root, pass_dir)?;
    let status = if promoted.is_some() {
        "no-go-promoted-design-tasks"
    } else {
        "no-go-no-new-design-tasks"
    };
    write_design_no_go_resolution_status(
        output_root,
        repo_root,
        pass,
        max_passes,
        pass_dir,
        status,
    )?;
    Ok(promoted)
}

fn write_design_no_go_resolution_status(
    output_root: &Path,
    repo_root: &Path,
    pass: usize,
    max_passes: usize,
    pass_dir: &Path,
    status: &str,
) -> Result<()> {
    let markdown = format!(
        "# Design Resolve Status\n\n- Status: `{status}`\n- Pass: `{pass}/{max_passes}`\n- Latest artifacts: `{}`\n- Latest report: `{}`\n- Design plan items: `{}`\n- Executor queue: `{}`\n- Recovery: final NO-GO preserved design repair work in the executor queue when parser-visible tasks were present; otherwise inspect the latest report and plan-items artifact for blockers.\n",
        pass_dir.display(),
        pass_dir.join("DESIGN-REPORT.md").display(),
        pass_dir.join("DESIGN-PLAN-ITEMS.md").display(),
        repo_root.join("IMPLEMENTATION_PLAN.md").display(),
    );
    atomic_write(
        &output_root.join("DESIGN-RESOLVE-STATUS.md"),
        markdown.as_bytes(),
    )
    .with_context(|| {
        format!(
            "failed to write {}",
            output_root.join("DESIGN-RESOLVE-STATUS.md").display()
        )
    })
}

pub(crate) async fn run_super_design_module(
    args: &SuperArgs,
    repo_root: &Path,
    planning_root: &Path,
    super_root: &Path,
) -> Result<()> {
    if !args.no_execute && args.design_resolve_passes > 1 {
        let design_args = DesignArgs {
            prompt: args.prompt.clone().or_else(|| args.focus.clone()),
            planning_root: Some(planning_root.to_path_buf()),
            output_dir: Some(super_root.join("design")),
            apply: true,
            resolve: true,
            resolve_passes: args.design_resolve_passes,
            max_concurrent_workers: args.max_concurrent_workers.max(1),
            max_iterations: args.max_iterations,
            worker_model: args.worker_model.clone(),
            worker_reasoning_effort: args.worker_reasoning_effort.clone(),
            branch: args.branch.clone(),
            reference_repos: args.reference_repos.clone(),
            skip_qa: false,
            model: args.model.clone(),
            reasoning_effort: args.reasoning_effort.clone(),
            codex_bin: args.codex_bin.clone(),
            dry_run: false,
        };
        return run_design_resolution(design_args, DesignRunKind::SuperResolve).await;
    }

    let design_root = super_root.join("design");
    fs::create_dir_all(&design_root)
        .with_context(|| format!("failed to create {}", design_root.display()))?;
    let prompt = build_design_prompt(
        repo_root,
        Some(planning_root),
        &design_root,
        args.prompt.as_deref().or(args.focus.as_deref()),
        true,
        false,
        DesignRunKind::Super,
    );
    let prompt_path = design_root.join("design-prompt.md");
    atomic_write(&prompt_path, prompt.as_bytes())
        .with_context(|| format!("failed to write {}", prompt_path.display()))?;
    run_design_codex_phase(
        repo_root,
        &design_root,
        &prompt,
        &args.model,
        &args.reasoning_effort,
        &args.codex_bin,
        "auto-super-design",
    )
    .await?;
    verify_design_artifacts(&design_root)?;
    require_design_go(&design_root)?;
    Ok(())
}

fn promote_design_plan_items_to_root_queue(
    repo_root: &Path,
    pass_dir: &Path,
) -> Result<Option<usize>> {
    let plan_items_path = pass_dir.join("DESIGN-PLAN-ITEMS.md");
    let root_plan_path = repo_root.join("IMPLEMENTATION_PLAN.md");
    if !plan_items_path.exists() || !root_plan_path.exists() {
        return Ok(None);
    }

    let plan_items = fs::read_to_string(&plan_items_path)
        .with_context(|| format!("failed to read {}", plan_items_path.display()))?;
    let mut root_plan = fs::read_to_string(&root_plan_path)
        .with_context(|| format!("failed to read {}", root_plan_path.display()))?;
    let blocks = extract_unchecked_design_plan_item_blocks(&plan_items);
    if blocks.is_empty() {
        return Ok(None);
    }

    let existing_task_ids = parse_tasks(&root_plan)
        .into_iter()
        .map(|task| task.id)
        .collect::<std::collections::BTreeSet<_>>();
    let mut missing = Vec::new();
    for block in blocks {
        let Some(task_id) = design_plan_block_task_id(&block) else {
            continue;
        };
        if !existing_task_ids.contains(&task_id) {
            missing.push(block);
        }
    }
    if missing.is_empty() {
        return Ok(None);
    }

    let insertion = format!(
        "\n<!-- auto design promoted unresolved design/runtime tasks from {} -->\n{}\n",
        plan_items_path.display(),
        missing.join("\n\n")
    );
    if let Some(index) = root_plan.find("\n## Follow-On Work") {
        root_plan.insert_str(index, &insertion);
    } else {
        if !root_plan.ends_with('\n') {
            root_plan.push('\n');
        }
        root_plan.push_str(&insertion);
    }
    atomic_write(&root_plan_path, root_plan.as_bytes())
        .with_context(|| format!("failed to write {}", root_plan_path.display()))?;
    Ok(Some(missing.len()))
}

fn extract_unchecked_design_plan_item_blocks(markdown: &str) -> Vec<String> {
    let mut blocks = Vec::new();
    let mut current = Vec::new();
    for line in markdown.lines() {
        if line.trim_start().starts_with("- [ ] `") || line.trim_start().starts_with("- [~] `") {
            if !current.is_empty() {
                blocks.push(current.join("\n"));
                current.clear();
            }
            current.push(line.to_string());
        } else if !current.is_empty() {
            current.push(line.to_string());
        }
    }
    if !current.is_empty() {
        blocks.push(current.join("\n"));
    }
    blocks
        .into_iter()
        .filter(|block| {
            let lower = block.to_ascii_lowercase();
            block.contains("Dependencies:")
                && block.contains("Verification:")
                && (lower.contains("runtime owner")
                    || lower.contains("source of truth")
                    || lower.contains("ui consumer"))
        })
        .collect()
}

fn design_plan_block_task_id(block: &str) -> Option<String> {
    let header = block.lines().next()?.trim_start();
    let rest = header
        .strip_prefix("- [ ] `")
        .or_else(|| header.strip_prefix("- [~] `"))?;
    let end = rest.find('`')?;
    Some(rest[..end].trim().to_string())
}

async fn run_design_codex_phase(
    repo_root: &Path,
    output_dir: &Path,
    prompt: &str,
    model: &str,
    reasoning_effort: &str,
    codex_bin: &Path,
    context_label: &str,
) -> Result<()> {
    let stderr_path = output_dir.join(format!("{context_label}-stderr.log"));
    println!("phase:       {context_label}");
    println!("stderr log:  {}", stderr_path.display());
    let status = run_codex_exec_max_context(
        repo_root,
        prompt,
        model,
        reasoning_effort,
        codex_bin,
        &stderr_path,
        None,
        context_label,
    )
    .await?;
    if !status.success() {
        bail!(
            "{context_label} failed with status {status}; see {}",
            stderr_path.display()
        );
    }
    Ok(())
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum DesignRunKind {
    Standalone,
    Resolve,
    Super,
    SuperResolve,
}

fn build_design_prompt(
    repo_root: &Path,
    planning_root: Option<&Path>,
    output_dir: &Path,
    operator_prompt: Option<&str>,
    apply: bool,
    skip_qa: bool,
    kind: DesignRunKind,
) -> String {
    let planning_clause = planning_root
        .map(|path| {
            format!(
                "- Planning corpus root: `{}`. If present, treat its `DESIGN.md` as planning input, not automatically as live product truth.",
                path.display()
            )
        })
        .unwrap_or_else(|| "- Planning corpus root: none detected.".to_string());
    let prompt_clause = operator_prompt
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|value| format!("\nOperator focus:\n{value}\n"))
        .unwrap_or_default();
    let edit_clause = if apply {
        match kind {
            DesignRunKind::Standalone => {
                "- You may make bounded edits to root `DESIGN.md`, design-relevant `specs/*.md`, and `IMPLEMENTATION_PLAN.md` when they are necessary to encode design/runtime truth. Do not edit application source code."
            }
            DesignRunKind::Resolve => {
                "- You may make bounded edits to root `DESIGN.md`, design-relevant `specs/*.md`, and `IMPLEMENTATION_PLAN.md` when they are necessary to encode design/runtime truth. Do not edit application source code in this design pass; unresolved source/runtime work must become executable implementation-plan tasks for the following `auto parallel` pass."
            }
            DesignRunKind::Super => {
                "- You may amend root `DESIGN.md` and the planning corpus design files so `auto gen` inherits the design contract. Do not edit source code, root specs, or root `IMPLEMENTATION_PLAN.md` in this pre-generation super module."
            }
            DesignRunKind::SuperResolve => {
                "- You may make bounded edits to root `DESIGN.md`, design-relevant `specs/*.md`, planning corpus design files, and `IMPLEMENTATION_PLAN.md` when needed to encode design/runtime truth. Do not edit application source code in this design pass; unresolved source/runtime work must become executable implementation-plan tasks for the following `auto parallel` pass."
            }
        }
    } else {
        "- Report-only mode: do not edit repo files outside the output directory. Put proposed patches and plan items in the artifacts."
    };
    let qa_clause = if skip_qa {
        "- Browser/runtime QA is explicitly skipped. Still inspect code-level UI/runtime contracts and list the skipped QA as a blocker where it matters."
    } else {
        "- Run the narrowest truthful frontend QA available: existing browser/Playwright/gstack/agent-browser tooling, local dev server smoke, route/API probes, console-error checks, and responsive checks. If no app can run, record the exact blocker and still audit static frontend/runtime bindings."
    };
    let stage_clause = match kind {
        DesignRunKind::Standalone => "You are running standalone `auto design`.",
        DesignRunKind::Resolve => {
            "You are running `auto design --resolve`: diagnose design/runtime drift, encode durable doctrine and queue-ready implementation tasks, then let implementation lanes repair source code before you re-verify."
        }
        DesignRunKind::Super => {
            "You are the `auto super` design perfection gate running after corpus and before generation. Design is first-class and blocking: do not subordinate, soften, or defer design/runtime integrity findings into a later generic review."
        }
        DesignRunKind::SuperResolve => {
            "You are the `auto super` design repair gate running after corpus and before generation. Design is first-class and blocking: diagnose design/runtime drift, encode executable repair work, let implementation lanes fix it, and only allow the CEO production campaign to continue after `Verdict: GO`."
        }
    };

    let planning_root_display = planning_root
        .map(|path| path.display().to_string())
        .unwrap_or_else(|| "no planning corpus".to_string());
    let apply_status = if apply {
        "applying edits is enabled"
    } else {
        "report-only mode is enabled"
    };

    let role = format!(
        "{stage_clause}\n\n\
         Repository: `{repo_root}`\n\
         {planning_clause}\n\
         Output directory: `{output_dir}`\n\
         {prompt_clause}\n\
         Your job is to synthesize expert design review, design-system consultation, web interface guidelines, \
         frontend design craft, and QA into a repo-native design contract. This is not a fake mockup generator. \
         This is a design/runtime integrity pass that must be perfected before broader functional lanes proceed.",
        stage_clause = stage_clause,
        repo_root = repo_root.display(),
        planning_clause = planning_clause,
        output_dir = output_dir.display(),
        prompt_clause = prompt_clause,
    );

    let lenses = "Use these lenses together:\n\
- Plan design review: rate and close gaps in information architecture, interaction states, journey, AI-slop risk, design-system alignment, responsive behavior, accessibility, and unresolved design decisions.\n\
- Design consultation: infer or improve a coherent product-specific system: aesthetic direction, safe category conventions, deliberate creative risks, typography, color, spacing, layout, motion, and component vocabulary.\n\
- Web interface guidelines: fetch or recall current web UI/a11y best practices and apply them to actual frontend files, not generic screenshots.\n\
- Frontend design craft: avoid generic AI aesthetics, overused fonts, purple-gradient defaults, meaningless cards, generic dashboard widgets, and product-copy fog. Existing design tokens and component patterns outrank generic advice.\n\
- QA discipline: test what a real user can do, check console/runtime errors after interactions, verify responsive states, and capture evidence or exact blockers.\n\
- Additional skills.sh design synthesis: use product-frontend critique for message clarity, frontend-ui-ux engineering for accessible polish and micro-interactions, and design-token extraction discipline from design-system skills. Do not require external paid design tools or infinite-canvas mockup systems.";

    let first_reads = format!(
        "Required first reads:\n\
- `AGENTS.md` or repo-local agent instructions.\n\
- Product doctrine: `README.md`, `DESIGN.md`, GDD/OS/invariant docs when present.\n\
- Planning truth: `IMPLEMENTATION_PLAN.md`, `REVIEW.md`, active `specs/`, and `{planning_root_display}` when present.\n\
- Frontend code: app/routes/components/styles/design tokens/tests/build scripts.\n\
- Runtime/engine/API code that owns facts displayed by UI.\n\
- Generated bindings/schemas/client code and their regeneration commands when present.",
    );

    let hard_rules = "Hard rules:\n\
- Do not create fake mockups as acceptance evidence. Preview pages are allowed only as proposals and must be labeled non-authoritative.\n\
- Do not invent frontend bindings, constants, catalogs, balances, settlement math, eligibility rules, risk classes, or status derivations. UI must consume runtime/API/generated truth.\n\
- If the design calls for new data, name the runtime owner, API/schema change, generator, consumer, and test/readback proof.\n\
- Prefer existing helpers, generated clients, hooks, stores, route loaders, and design tokens over new manual glue.\n\
- Production code must not import fixture/demo/sample data as fallback truth.\n\
- Retired or superseded screens/specs must be deleted, archived, tombstoned, or explicitly blocked from active implementation.\n\
- A design improvement is not complete unless it names the engine/API contract and the proof that would fail if UI drifts again.";

    let artifacts = format!(
        "Write these non-empty artifacts under `{output_dir}`:\n\
1. `DESIGN-AUDIT.md`\n\
   - Current UI/design-system inventory.\n\
   - Existing frontend design signals and reusable components/tokens.\n\
   - 0-10 ratings for the seven plan-design-review dimensions.\n\
   - AI-slop risks and modern/stunning UI opportunities specific to this product.\n\
2. `DESIGN-SYSTEM-PROPOSAL.md`\n\
   - Proposed or revised `DESIGN.md` doctrine.\n\
   - Aesthetic thesis, safe choices, deliberate risks, typography, color, spacing, layout, motion, components, empty/error/loading states, responsive and accessibility rules.\n\
   - Explicitly explain what belongs in real product UI versus non-authoritative concept previews.\n\
3. `ENGINE-UI-CONTRACT.md`\n\
   - Table of UI surfaces, runtime/API source of truth, existing helpers/bindings, generated artifacts, fixture boundary, and required drift guard.\n\
   - Call out every manual binding or duplicated frontend derivation found.\n\
4. `FRONTEND-QA.md`\n\
   - Commands/URLs/tools used, screenshots or artifact paths if produced, console/runtime findings, responsive findings, and exact blockers.\n\
   - Separate confirmed breaks from hypotheses and from skipped/unavailable checks.\n\
5. `DESIGN-PLAN-ITEMS.md`\n\
   - Queue-ready plan items for unresolved design/runtime gaps using the repo's implementation-plan field style.\n\
   - Every item must include runtime owner, UI consumers, generated artifacts, contract generation, cross-surface proof, and closeout review.\n\
6. `DESIGN-REPORT.md`\n\
   - Executive summary, files changed if any, recommended next workflow step, and GO/NO-GO for design-aware implementation.\n\
   - In the `auto super` flow, `Verdict: NO-GO` blocks the CEO production campaign until design/runtime integrity is repaired.",
        output_dir = output_dir.display(),
    );

    // Progress-detection contract: every NO-GO report MUST carry a structured
    // blockers section and a `kind:` annotation per blocker. The host
    // (`fingerprint_design_report`) hashes this section pass-to-pass and
    // short-circuits identical retries. Without these annotations the host
    // cannot distinguish "external runtime data drift" (re-running design
    // cannot affect this) from "design quality blocker" (another pass might
    // close it), and it cannot prove progress between passes.
    let progress_contract = "Required blockers contract (NO-GO only):\n\
- Append a `## Required Blockers Before Merge` section to `DESIGN-REPORT.md` whenever the verdict is NO-GO.\n\
- One row per blocker, format: `- [ ] DESIGN-<stable-id> kind: <kind> -- <one-line summary>`.\n\
- `kind:` MUST be one of:\n  \
* `design-quality` -- a doctrine, IA, copy, accessibility, token, or runtime/UI contract gap that another design pass can plausibly close.\n  \
* `external-runtime-data` -- repo-external state design re-runs cannot fix (operated readback red, strict audit blocker depending on data outside this commit, browser proof red because a service is down, fixture freshness drift, etc.).\n\
- A NO-GO whose blockers are all `external-runtime-data` triggers a short-circuit: re-running the design gate cannot move them, so it must not be retried in a loop. List them honestly so the host can short-circuit instead of burning the retry budget.\n\
- A NO-GO must list at least one `design-quality` blocker if any actually exist. Do not down-rank a real design issue to `external-runtime-data` to escape the gate.\n\
- Use stable IDs (`DESIGN-<datestamp>-<n>` matching the IDs you used in `DESIGN-PLAN-ITEMS.md` and `IMPLEMENTATION_PLAN.md`). Re-emit the same ID across passes when the blocker is unchanged so the host can detect zero forward progress.";

    let apply_clause = format!(
        "If `{apply_status}`:\n\
- Update `DESIGN.md` only with durable doctrine grounded in the live product and existing frontend.\n\
- In standalone mode, add or amend plan/spec items only for real unresolved work. In super mode, prefer amending the planning corpus so `auto gen` emits the queue unless this is a resolve pass.\n\
- In resolve mode, every unresolved NO-GO issue that requires source/runtime/UI changes must also be inserted into root `IMPLEMENTATION_PLAN.md` as an unchecked, dependency-ready task unless it has a concrete dependency. Use stable `DESIGN-*` task IDs, machine-readable `Dependencies:`, narrow `Owns:`, runtime owner, UI consumer, generated artifact, fixture boundary, and executable verification fields so `auto parallel` can pick it up immediately.\n\
- In resolve mode, do not leave the only actionable repair work inside `DESIGN-PLAN-ITEMS.md`; that file is an audit artifact, while `IMPLEMENTATION_PLAN.md` is the executor queue.\n\
- Do not mark any implementation item complete.",
    );

    let edit_boundary = format!("{edit_clause}\n{qa_clause}");

    let spec = PromptSpec::new(role)
        .ethos(EthosPosture::Full)
        .edit_boundary(edit_boundary)
        .input("Lenses", lenses)
        .input("First reads", first_reads)
        .input("Hard rules", hard_rules)
        .output("Required artifacts", artifacts)
        .output("Required blockers contract", progress_contract)
        .output("Apply mode contract", apply_clause)
        .verdicts(["Verdict: GO", "Verdict: NO-GO"]);

    // Record which model tier this stage is expected to use. Default per
    // `PipelineStage::DesignGate` is `PromptTier::Final` (Opus-class) -- design
    // is high-stakes and not a candidate for cheaper tiers. The resolved alias
    // is surfaced as a sanity-check comment so operator diffs of the prompt log
    // see which tier rendered.
    let tier: PromptTier = PipelineStage::DesignGate.default_tier();
    let tier_note = format!(
        "<!-- design-gate tier={:?} claude_model={} codex_model={} effort={} -->",
        tier,
        tier.claude_model(),
        tier.codex_model(),
        tier.effort()
    );

    format!("{tier_note}\n\n{}", spec.render())
}

fn verify_design_artifacts(output_dir: &Path) -> Result<()> {
    for artifact in DESIGN_ARTIFACTS {
        require_nonempty_file(&output_dir.join(artifact))?;
    }
    let report_path = output_dir.join("DESIGN-REPORT.md");
    let report = fs::read_to_string(&report_path)
        .with_context(|| format!("failed to read {}", report_path.display()))?;
    if exact_terminal_verdict(&report, &["Verdict: GO", "Verdict: NO-GO"])?.is_none() {
        bail!(
            "{} must contain `Verdict: GO` or `Verdict: NO-GO`",
            report_path.display()
        );
    }
    Ok(())
}

fn require_design_go(output_dir: &Path) -> Result<()> {
    if design_report_is_go(output_dir)? {
        return Ok(());
    }
    let report_path = output_dir.join("DESIGN-REPORT.md");
    bail!(
        "design perfection gate did not approve downstream generation; expected `Verdict: GO` in {}",
        report_path.display()
    );
}

fn design_report_is_go(output_dir: &Path) -> Result<bool> {
    let report_path = output_dir.join("DESIGN-REPORT.md");
    let report = fs::read_to_string(&report_path)
        .with_context(|| format!("failed to read {}", report_path.display()))?;
    Ok(terminal_verdict_is(
        &report,
        "Verdict: GO",
        &["Verdict: GO", "Verdict: NO-GO"],
    ))
}

fn require_nonempty_file(path: &Path) -> Result<()> {
    let metadata = fs::metadata(path)
        .with_context(|| format!("required design artifact missing: {}", path.display()))?;
    if metadata.len() == 0 {
        bail!("required design artifact is empty: {}", path.display());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{
        build_design_prompt, enforce_design_report_only_write_boundary,
        preserve_final_no_go_design_plan_items, promote_design_plan_items_to_root_queue,
        DesignRunKind,
    };
    use crate::qa_only_command::{collect_dirty_state, format_final_status_block};
    use crate::task_parser::{parse_tasks, TaskStatus};
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn design_prompt_rejects_fake_mockup_and_requires_runtime_truth() {
        let prompt = build_design_prompt(
            &PathBuf::from("/repo"),
            Some(&PathBuf::from("/repo/genesis")),
            &PathBuf::from("/repo/.auto/design/run"),
            Some("make the UI better"),
            true,
            false,
            DesignRunKind::Standalone,
        );

        assert!(prompt.contains("not a fake mockup generator"));
        assert!(prompt.contains("Do not create fake mockups as acceptance evidence"));
        assert!(prompt.contains("UI must consume runtime/API/generated truth"));
        assert!(prompt.contains("ENGINE-UI-CONTRACT.md"));
        assert!(prompt.contains("FRONTEND-QA.md"));
    }

    #[test]
    fn design_report_only_rejects_disallowed_dirty_state() {
        let root = temp_dir("design-report-only-boundary");
        run_git_in(&root, ["init"]);
        run_git_in(&root, ["config", "user.name", "autodev tests"]);
        run_git_in(&root, ["config", "user.email", "autodev@example.com"]);
        fs::write(root.join("README.md"), "# temp\n").unwrap();
        run_git_in(&root, ["add", "README.md"]);
        run_git_in(&root, ["commit", "-m", "init"]);
        let output_dir = root.join(".auto/design/run");
        fs::create_dir_all(&output_dir).unwrap();
        let baseline = collect_dirty_state(&root).unwrap();

        fs::create_dir_all(root.join("src")).unwrap();
        fs::write(root.join("src/lib.rs"), "pub fn changed() {}\n").unwrap();

        let err = enforce_design_report_only_write_boundary(&root, &output_dir, &baseline)
            .expect_err("source edits should violate report-only design boundary");
        assert!(err.to_string().contains("write boundary violation"));
        assert!(err.to_string().contains("src/lib.rs"));
    }

    #[test]
    fn design_final_status_block_names_operator_contract_fields() {
        let block = format_final_status_block(
            "design artifacts verified",
            &[".auto/design/run/DESIGN-REPORT.md".to_string()],
            "none",
            "review DESIGN-REPORT.md verdict",
        );

        assert!(block.contains("status:"));
        assert!(block.contains("files written:"));
        assert!(block.contains("blockers:"));
        assert!(block.contains("next step:"));
        assert!(block.contains("DESIGN-REPORT.md"));
    }

    #[test]
    fn super_design_prompt_keeps_pre_generation_edit_boundary() {
        let prompt = build_design_prompt(
            &PathBuf::from("/repo"),
            Some(&PathBuf::from("/repo/genesis")),
            &PathBuf::from("/repo/.auto/super/run/design"),
            None,
            true,
            false,
            DesignRunKind::Super,
        );

        assert!(prompt.contains("auto super` design perfection gate"));
        assert!(prompt.contains("Design is first-class and blocking"));
        assert!(prompt
            .contains("Do not edit source code, root specs, or root `IMPLEMENTATION_PLAN.md`"));
    }

    #[test]
    fn design_plan_items_promote_missing_executor_tasks_to_root_queue() {
        let root = temp_dir("design-plan-promotion");
        let pass_dir = root.join(".auto/design/pass-01");
        fs::create_dir_all(&pass_dir).unwrap();
        fs::write(
            root.join("IMPLEMENTATION_PLAN.md"),
            "# IMPLEMENTATION_PLAN\n\n## Priority Work\n\n## Follow-On Work\n\n",
        )
        .unwrap();
        fs::write(
            pass_dir.join("DESIGN-PLAN-ITEMS.md"),
            "- [ ] `DESIGN-001` Runtime-backed surface\n\n    Runtime owner: `src/api.rs`\n    UI consumers: `src/App.tsx`\n    Verification: `cargo test design_001`\n    Dependencies: none\n",
        )
        .unwrap();

        assert_eq!(
            promote_design_plan_items_to_root_queue(&root, &pass_dir).unwrap(),
            Some(1)
        );
        let root_plan = fs::read_to_string(root.join("IMPLEMENTATION_PLAN.md")).unwrap();
        assert!(root_plan.contains("`DESIGN-001`"));
        assert!(
            root_plan.find("`DESIGN-001`").unwrap() < root_plan.find("## Follow-On Work").unwrap()
        );
        let tasks = parse_tasks(&root_plan);
        let promoted = tasks
            .iter()
            .find(|task| task.id == "DESIGN-001")
            .expect("promoted design task should be parser-visible");
        assert_eq!(promoted.status, TaskStatus::Pending);
        assert!(promoted.dependencies.is_empty());

        assert_eq!(
            promote_design_plan_items_to_root_queue(&root, &pass_dir).unwrap(),
            None
        );
    }

    #[test]
    fn final_no_go_promotes_design_tasks_before_failure() {
        let root = temp_dir("design-final-no-go-promotion");
        let output_root = root.join(".auto/design/run");
        let pass_dir = output_root.join("pass-01");
        fs::create_dir_all(&pass_dir).unwrap();
        fs::write(
            root.join("IMPLEMENTATION_PLAN.md"),
            "# IMPLEMENTATION_PLAN\n\n## Priority Work\n\n## Follow-On Work\n\n",
        )
        .unwrap();
        fs::write(
            pass_dir.join("DESIGN-REPORT.md"),
            "Remaining design/runtime gaps.\n\nVerdict: NO-GO\n",
        )
        .unwrap();
        fs::write(
            pass_dir.join("DESIGN-PLAN-ITEMS.md"),
            "- [ ] `DESIGN-999` Final NO-GO repair\n\n    Source of truth: `src/design_command.rs`\n    Runtime owner: `src/design_command.rs`\n    UI consumers: root `IMPLEMENTATION_PLAN.md`\n    Generated artifacts: `.auto/design/run/pass-01/DESIGN-PLAN-ITEMS.md`, root `IMPLEMENTATION_PLAN.md`\n    Fixture boundary: tests use temporary pass directories only.\n    Verification: `cargo test design_command::tests::final_no_go_promotes_design_tasks_before_failure`\n    Dependencies: none\n",
        )
        .unwrap();

        assert_eq!(
            preserve_final_no_go_design_plan_items(&root, &output_root, 1, 1, &pass_dir).unwrap(),
            Some(1)
        );

        let root_plan = fs::read_to_string(root.join("IMPLEMENTATION_PLAN.md")).unwrap();
        let tasks = parse_tasks(&root_plan);
        let promoted = tasks
            .iter()
            .find(|task| task.id == "DESIGN-999")
            .expect("final NO-GO design task should be parser-visible");
        assert_eq!(promoted.status, TaskStatus::Pending);
        assert!(promoted.dependencies.is_empty());

        let status = fs::read_to_string(output_root.join("DESIGN-RESOLVE-STATUS.md")).unwrap();
        assert!(status.contains("no-go-promoted-design-tasks"));
        assert!(status.contains("DESIGN-PLAN-ITEMS.md"));

        assert_eq!(
            preserve_final_no_go_design_plan_items(&root, &output_root, 1, 1, &pass_dir).unwrap(),
            None
        );
    }

    #[test]
    fn final_no_go_existing_root_repair_task_is_recoverable() {
        let root = temp_dir("design-final-no-go-existing-repair");
        fs::write(
            root.join("IMPLEMENTATION_PLAN.md"),
            "# IMPLEMENTATION_PLAN\n\n## Priority Work\n\n- [x] `DESIGN-001` Done\n    Dependencies: none\n\n- [ ] `DESIGN-008` Active ledger reconciliation before generation\n    Dependencies: `DESIGN-001`\n\n## Follow-On Work\n\n",
        )
        .unwrap();

        assert!(super::root_queue_has_dependency_ready_repair_tasks(&root).unwrap());
    }

    #[test]
    fn final_no_go_blocked_root_repair_task_is_not_recoverable() {
        let root = temp_dir("design-final-no-go-blocked-repair");
        fs::write(
            root.join("IMPLEMENTATION_PLAN.md"),
            "# IMPLEMENTATION_PLAN\n\n## Priority Work\n\n- [ ] `DESIGN-008` Active ledger reconciliation before generation\n    Dependencies: `DESIGN-007`\n\n## Follow-On Work\n\n",
        )
        .unwrap();

        assert!(!super::root_queue_has_dependency_ready_repair_tasks(&root).unwrap());
    }

    fn temp_dir(label: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!("autodev-{label}-{nanos}"));
        fs::create_dir_all(&path).unwrap();
        path
    }

    fn run_git_in<'a>(repo: &Path, args: impl IntoIterator<Item = &'a str>) {
        let output = std::process::Command::new("git")
            .arg("-C")
            .arg(repo)
            .args(args)
            .output()
            .expect("failed to launch git");
        assert!(
            output.status.success(),
            "git failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    // ---- Progress detection (Change #7) ---------------------------------

    use super::{
        all_blockers_external_runtime_data, canonicalize_blocker_stem, fingerprint_design_report,
        record_pass_progress, ProgressOutcome,
    };

    const REPORT_PASS_1: &str = "# Design Report\n\
\n\
Verdict: NO-GO above.\n\
\n\
## Required Blockers Before Merge\n\
- [ ] DESIGN-120526-01 kind: design-quality -- restore composition packet\n\
- [ ] DESIGN-120526-06 kind: design-quality -- web build broken on fixture drift\n\
\n\
Verdict: NO-GO\n";

    const REPORT_PASS_2_SAME: &str = "# Design Report\n\
\n\
Pass 2 of design resolve.\n\
\n\
## Required Blockers Before Merge\n\
- [ ] DESIGN-120526-06 kind: design-quality -- web build broken on fixture drift\n\
- [ ] DESIGN-120526-01 kind: design-quality -- restore composition packet (date 2026-05-13)\n\
\n\
Verdict: NO-GO\n";

    const REPORT_PASS_2_DIFFERENT: &str = "# Design Report\n\
\n\
## Required Blockers Before Merge\n\
- [ ] DESIGN-120526-09 kind: design-quality -- new IA gap surfaced after repair\n\
\n\
Verdict: NO-GO\n";

    const REPORT_ALL_EXTERNAL: &str = "# Design Report\n\
\n\
## Required Blockers Before Merge\n\
- [ ] DESIGN-120526-21 kind: external-runtime-data -- operated Loom 502\n\
- [ ] DESIGN-120526-22 kind: external-runtime-data -- strict audit red without --allow-blockers\n\
\n\
Verdict: NO-GO\n";

    #[test]
    fn fingerprint_is_stable_for_same_blocker_set() {
        let fp_a = fingerprint_design_report(REPORT_PASS_1);
        let fp_b = fingerprint_design_report(REPORT_PASS_2_SAME);
        assert_eq!(
            fp_a.digest, fp_b.digest,
            "reordered + date-noise blockers should fold to the same fingerprint; got {fp_a:?} vs {fp_b:?}"
        );
        assert_ne!(fp_a.digest, "unstructured");
        assert_eq!(
            fp_a.blocker_ids,
            vec!["DESIGN-120526-01", "DESIGN-120526-06"]
        );
        assert_eq!(fp_a.blocker_count, 2);
        assert_eq!(fp_a.kinds, vec!["design-quality"]);
    }

    #[test]
    fn fingerprint_changes_when_blocker_ids_change() {
        let fp_a = fingerprint_design_report(REPORT_PASS_1);
        let fp_b = fingerprint_design_report(REPORT_PASS_2_DIFFERENT);
        assert_ne!(fp_a.digest, fp_b.digest);
        assert_eq!(fp_b.blocker_ids, vec!["DESIGN-120526-09"]);
    }

    #[test]
    fn fingerprint_returns_unstructured_for_freeform_reports() {
        let fp = fingerprint_design_report("# Design Report\n\nVerdict: NO-GO\n");
        assert_eq!(fp.digest, "unstructured");
        assert!(fp.blocker_ids.is_empty());
        assert_eq!(fp.blocker_count, 0);
    }

    #[test]
    fn canonical_stem_strips_date_and_hash_noise() {
        let a = canonicalize_blocker_stem(
            "- [ ] DESIGN-120526-01 kind: design-quality -- foo 2026-05-12 abcdef1234",
        );
        let b = canonicalize_blocker_stem(
            "DESIGN-120526-01 kind: design-quality -- foo 20260513 99dead8877",
        );
        assert_eq!(a, b, "date and hex hash should be stripped before hashing");
        assert!(a.contains("kind: design-quality"));
    }

    #[test]
    fn record_pass_progress_first_pass_progresses() {
        let dir = temp_dir("design-progress-first-pass");
        let progress = dir.join("progress.json");
        let fp = fingerprint_design_report(REPORT_PASS_1);
        let outcome = record_pass_progress(&progress, 1, "NO-GO", &fp).unwrap();
        assert!(matches!(outcome, ProgressOutcome::Progressed));
        assert!(progress.exists(), "progress.json should be persisted");
        let raw = fs::read_to_string(&progress).unwrap();
        assert!(raw.contains("\"pass\": 1"));
        assert!(raw.contains("\"verdict\": \"NO-GO\""));
        assert!(raw.contains("\"blocker_count\": 2"));
    }

    #[test]
    fn record_pass_progress_detects_stall_on_identical_fingerprint() {
        let dir = temp_dir("design-progress-stall");
        let progress = dir.join("progress.json");
        let fp1 = fingerprint_design_report(REPORT_PASS_1);
        let outcome1 = record_pass_progress(&progress, 1, "NO-GO", &fp1).unwrap();
        assert!(matches!(outcome1, ProgressOutcome::Progressed));

        let fp2 = fingerprint_design_report(REPORT_PASS_2_SAME);
        let outcome2 = record_pass_progress(&progress, 2, "NO-GO", &fp2).unwrap();
        match outcome2 {
            ProgressOutcome::Stalled { prior_pass } => assert_eq!(prior_pass, 1),
            ProgressOutcome::Progressed => panic!("expected Stalled, got Progressed"),
            ProgressOutcome::AllExternalRuntimeData => {
                panic!("expected Stalled, got AllExternalRuntimeData")
            }
        }
    }

    #[test]
    fn record_pass_progress_continues_when_fingerprint_differs() {
        let dir = temp_dir("design-progress-continue");
        let progress = dir.join("progress.json");
        let fp1 = fingerprint_design_report(REPORT_PASS_1);
        record_pass_progress(&progress, 1, "NO-GO", &fp1).unwrap();
        let fp2 = fingerprint_design_report(REPORT_PASS_2_DIFFERENT);
        let outcome = record_pass_progress(&progress, 2, "NO-GO", &fp2).unwrap();
        assert!(matches!(outcome, ProgressOutcome::Progressed));
        let raw = fs::read_to_string(&progress).unwrap();
        // Two pass entries persisted.
        assert_eq!(raw.matches("\"pass\":").count(), 2);
    }

    #[test]
    fn record_pass_progress_short_circuits_on_all_external_runtime_data() {
        let dir = temp_dir("design-progress-external");
        let progress = dir.join("progress.json");
        let fp = fingerprint_design_report(REPORT_ALL_EXTERNAL);
        assert!(all_blockers_external_runtime_data(&fp));
        let outcome = record_pass_progress(&progress, 1, "NO-GO", &fp).unwrap();
        assert!(matches!(outcome, ProgressOutcome::AllExternalRuntimeData));
    }

    #[test]
    fn record_pass_progress_does_not_short_circuit_mixed_kinds() {
        let fp = fingerprint_design_report(
            "# Design Report\n\n## Required Blockers Before Merge\n\
             - [ ] DESIGN-1 kind: external-runtime-data -- service down\n\
             - [ ] DESIGN-2 kind: design-quality -- IA gap\n\n\
             Verdict: NO-GO\n",
        );
        assert!(!all_blockers_external_runtime_data(&fp));
    }

    #[test]
    fn unstructured_reports_do_not_stall_loop() {
        let dir = temp_dir("design-progress-unstructured");
        let progress = dir.join("progress.json");
        let fp1 = fingerprint_design_report("# Design Report\n\nVerdict: NO-GO\n");
        let fp2 = fingerprint_design_report("# Design Report\n\nVerdict: NO-GO\n");
        assert_eq!(fp1.digest, "unstructured");
        record_pass_progress(&progress, 1, "NO-GO", &fp1).unwrap();
        let outcome = record_pass_progress(&progress, 2, "NO-GO", &fp2).unwrap();
        assert!(
            matches!(outcome, ProgressOutcome::Progressed),
            "two unstructured reports should not trigger a stall (we cannot prove no-progress)",
        );
    }

    #[test]
    fn design_prompt_emits_required_blockers_contract_and_tier_metadata() {
        let prompt = build_design_prompt(
            &PathBuf::from("/repo"),
            None,
            &PathBuf::from("/repo/.auto/design/run"),
            None,
            true,
            false,
            DesignRunKind::Resolve,
        );
        assert!(
            prompt.contains("Required Blockers Before Merge"),
            "prompt should instruct the worker to emit the canonical blockers section"
        );
        assert!(prompt.contains("design-quality"));
        assert!(prompt.contains("external-runtime-data"));
        assert!(prompt.contains("Verdict: GO"));
        assert!(prompt.contains("Verdict: NO-GO"));
        assert!(prompt.contains("design-gate tier=Final"));
    }
}
