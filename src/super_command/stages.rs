//! Super stages: CEO corpus review, execution-gate review, and the Codex phase runner.

use std::fs;
use std::path::Path;

use anyhow::{bail, Context, Result};

use crate::codex_exec::run_codex_exec_max_context;
use crate::super_command::{
    EXECUTION_GATE_FILE, SUPER_EXECUTION_GATE_VERDICTS, SUPER_REPORT_FILES,
};
use crate::util::atomic_write;
use crate::verdict::exact_terminal_verdict;
use crate::SuperArgs;

pub(crate) fn build_super_focus(prompt: Option<&str>, focus: Option<&str>) -> String {
    let mut parts = Vec::new();
    parts.push(
        "You are the new CEO inheriting this codebase. Over the next 14 days, race it to production with unlimited compute and resources. Do not capacity-trim the ambition: prioritize the deliverables that maximize production readiness, then assume max parallel execution can attack them. Perfect design/runtime integrity first, then run equally rigorous functional reviews across product, engineering, security, reliability, QA, data/contracts, operations, release, DX, and performance. Keep auto corpus and auto gen as the control primitives, but shape the corpus toward release blockers, operator trust, verification evidence, first-run DX, and maintainable execution contracts.",
    );
    if let Some(prompt) = prompt.filter(|value| !value.trim().is_empty()) {
        parts.push(prompt.trim());
    }
    if let Some(focus) = focus.filter(|value| !value.trim().is_empty()) {
        parts.push(focus.trim());
    }
    parts.join("\n\n")
}

pub(crate) async fn run_super_corpus_review(
    args: &SuperArgs,
    repo_root: &Path,
    planning_root: &Path,
    super_root: &Path,
) -> Result<()> {
    let prompt = build_super_corpus_review_prompt(repo_root, planning_root, super_root);
    run_super_codex_phase(
        repo_root,
        super_root,
        "super-corpus-review",
        &prompt,
        &args.model,
        &args.reasoning_effort,
        &args.codex_bin,
    )
    .await?;
    for file in SUPER_REPORT_FILES {
        require_nonempty_file(&super_root.join(file))?;
    }
    Ok(())
}

pub(crate) async fn run_super_execution_gate(
    args: &SuperArgs,
    repo_root: &Path,
    planning_root: &Path,
    output_dir: Option<&Path>,
    super_root: &Path,
) -> Result<()> {
    let prompt =
        build_super_execution_gate_prompt(repo_root, planning_root, output_dir, super_root);
    run_super_codex_phase(
        repo_root,
        super_root,
        "super-execution-gate",
        &prompt,
        &args.model,
        &args.reasoning_effort,
        &args.codex_bin,
    )
    .await?;
    let gate_path = super_root.join(EXECUTION_GATE_FILE);
    validate_super_execution_gate_report(&gate_path)
}

fn validate_super_execution_gate_report(gate_path: &Path) -> Result<()> {
    require_nonempty_file(gate_path)?;
    let gate = fs::read_to_string(gate_path)
        .with_context(|| format!("failed to read {}", gate_path.display()))?;
    let verdict = exact_terminal_verdict(&gate, &SUPER_EXECUTION_GATE_VERDICTS)
        .with_context(|| format!("invalid terminal verdict in {}", gate_path.display()))?;
    match verdict.as_deref() {
        Some("Verdict: GO") => Ok(()),
        Some(found) => bail!(
            "super execution gate did not approve parallel execution; found `{found}` in {}; expected exactly one `Verdict: GO` terminal verdict",
            gate_path.display()
        ),
        None => bail!(
            "super execution gate did not approve parallel execution; missing terminal verdict in {}; expected exactly one `Verdict: GO` or `Verdict: NO-GO`",
            gate_path.display()
        ),
    }
}

pub(crate) async fn run_super_codex_phase(
    repo_root: &Path,
    super_root: &Path,
    phase_slug: &str,
    prompt: &str,
    model: &str,
    reasoning_effort: &str,
    codex_bin: &Path,
) -> Result<()> {
    let prompt_path = super_root.join(format!("{phase_slug}-prompt.md"));
    let stderr_path = super_root.join(format!("{phase_slug}-stderr.log"));
    atomic_write(&prompt_path, prompt.as_bytes())
        .with_context(|| format!("failed to write {}", prompt_path.display()))?;
    println!("phase:       {phase_slug}");
    println!("model:       {model}");
    println!("effort:      {reasoning_effort}");
    println!("context:     max");
    println!("prompt log:  {}", prompt_path.display());
    println!("stderr log:  {}", stderr_path.display());
    let status = run_codex_exec_max_context(
        repo_root,
        prompt,
        model,
        reasoning_effort,
        codex_bin,
        &stderr_path,
        None,
        phase_slug,
    )
    .await?;
    if !status.success() {
        bail!(
            "super phase `{phase_slug}` failed with status {status}; see {}",
            stderr_path.display()
        );
    }
    Ok(())
}

fn build_super_corpus_review_prompt(
    repo_root: &Path,
    planning_root: &Path,
    super_root: &Path,
) -> String {
    format!(
        r#"You are the new CEO of this codebase running the `auto super` functional review war room.

The normal `auto corpus` authoring and review passes have already produced `{planning_root}` for the repository at `{repo_root}`. The design perfection gate may also have written design/runtime artifacts under `{super_root}/design`. Treat those design artifacts as the first production-readiness input, not as a subordinate style appendix.

Mission:
- You inherited this codebase today.
- You have 14 days to race it to production.
- Compute and implementation capacity are not constraints; prioritization is about production leverage, risk, and dependency order.
- Design/runtime integrity was perfected first. Now apply the same severity and precision across every functional lane.

Edit boundary:
- You may read the repository at `{repo_root}` and the planning corpus at `{planning_root}`.
- You may read `{super_root}/design` and should preserve its runtime-first design/UI findings when they exist.
- You may edit markdown files under `{planning_root}`.
- You must write these non-empty files under `{super_root}`:
  - `CEO-14-DAY-PLAN.md`
  - `FUNCTIONAL-REVIEWS.md`
  - `PRODUCTION-READINESS.md`
  - `RISK-REGISTER.md`
  - `QUALITY-GATES.md`
  - `SYSTEM-MAP.md`
  - `SUPER-REPORT.md`
- Do not edit source code, root specs, root implementation plans, generated `gen-*` dirs, or skill definition directories.

Run these functional reviews and synthesize their disagreements:
- CEO/Product: production definition, 10-star user outcome, non-goals, opportunity cost, scope discipline.
- Design/Frontend: design-system clarity, modern UI quality, accessibility, AI-slop risk, and runtime/UI drift; respect `{super_root}/design` as the opening gate.
- Principal Engineer/Architecture: architecture seams, data flow, state, dependency order, maintainability.
- Runtime/Engine: source-of-truth ownership, generated contracts, API/schema drift, state transitions, invariants.
- Security/Trust: credentials, shell/YAML injection, secrets, dangerous flags, logs, authz, trust boundaries.
- Reliability/Ops: idempotence, resume, partial failure, recovery, observability, receipts, operator handoff.
- QA/Test Architect: missing regression tests, integration proof, false-positive verification, browser/runtime evidence.
- Data/Contracts: migrations, compatibility, durable artifacts, schema ownership, backfill or rollback hazards.
- Performance/Scale: hot paths, large repos, concurrency, resource cleanup, timeout behavior.
- DX/Agent Workflow: first-run success, CLI help, errors, honest examples, setup friction, model/provider routing.
- Release Manager: CI, install proof, versioning, rollback, release blockers, ship/no-ship criteria.

Required output semantics:
- `CEO-14-DAY-PLAN.md` must define the 14-day production race, top outcomes, dependency waves, and prioritized deliverables without capacity trimming.
- `FUNCTIONAL-REVIEWS.md` must contain the lane-by-lane review board findings, severity, owner, needed artifact, and proof for each discipline above.
- `PRODUCTION-READINESS.md` must contain a matrix by major subsystem with grade, evidence, production blocker, required fix, and proof artifact/command.
- `RISK-REGISTER.md` must rank risks by severity, likelihood, blast radius, mitigation, and release-blocking status.
- `QUALITY-GATES.md` must define hard gates before parallel execution, before release candidate, and before ship.
- `SYSTEM-MAP.md` must map command surface, state files, external CLIs, credential flows, write paths, and generated artifacts.
- `SUPER-REPORT.md` must summarize top blockers, top non-blocking improvements, not-doing list, how design was handled first, functional-lane risks, and any amendments made to `{planning_root}`.

If the corpus under `{planning_root}` is missing production-readiness framing, amend it in place so the next `auto gen` pass produces release-oriented specs and executable plan tasks. Deliverables should be dependency-ordered for max-compute parallelism, not limited by a small team capacity assumption. Keep `genesis/` as corpus input, not a competing active control plane unless repository instructions explicitly say otherwise.
"#,
        repo_root = repo_root.display(),
        planning_root = planning_root.display(),
        super_root = super_root.display(),
    )
}

fn build_super_execution_gate_prompt(
    repo_root: &Path,
    planning_root: &Path,
    output_dir: Option<&Path>,
    super_root: &Path,
) -> String {
    let output_clause = output_dir
        .map(|path| path.display().to_string())
        .unwrap_or_else(|| "the latest gen output recorded in .auto/state.json".to_string());
    format!(
        r#"You are the final `auto super` execution gate before `auto parallel` launches.

The repository is `{repo_root}`. The planning corpus is `{planning_root}`. The generated output is `{output_clause}`. The super artifacts are under `{super_root}`.

Edit boundary:
- You may read the repository, `{planning_root}`, generated output, root `specs/`, and root `IMPLEMENTATION_PLAN.md`.
- You may read `{super_root}/design`; design/runtime UI contract risks are execution-gate inputs, not decoration.
- You must read `{super_root}/CEO-14-DAY-PLAN.md`, `{super_root}/FUNCTIONAL-REVIEWS.md`, `{super_root}/PRODUCTION-READINESS.md`, `{super_root}/RISK-REGISTER.md`, `{super_root}/QUALITY-GATES.md`, and `{super_root}/SYSTEM-MAP.md` when present.
- You may edit only `{super_root}/EXECUTION-GATE.md`.
- Do not edit root `IMPLEMENTATION_PLAN.md` or root `specs/*.md`; default `auto super` keeps root queue truth unchanged until the operator promotes the generated snapshot.
- Do not edit source code, `genesis/`, `gen-*`, skill definition directories, or worker artifacts.

Review the generated snapshot plan as a promotion candidate. Max-compute tmux-backed implementation workers cannot start from this super run until the operator promotes the snapshot with `auto gen --sync-only --output-dir <gen-dir>`.

Gate criteria:
- The queue must implement the CEO 14-day production race, not a generic cleanup backlog or capacity-trimmed wishlist.
- UI/design tasks must be tied to runtime/API source of truth, generated bindings, existing frontend helpers, and cross-surface readback proof. Reject fake mockups, manual frontend bindings, and fixture-data fallbacks as acceptance evidence.
- Security, reliability, QA, data/contracts, operations, release, DX, and performance lanes must receive the same severity and proof standard as design.
- Priority tasks must be dependency-ordered and small enough for one focused worker session.
- Every unfinished task must have concrete ownership, acceptance criteria, verification, required tests, completion artifacts, dependencies, estimated scope, and completion signal.
- Verification must be narrow and meaningful. Reject broad package-wide test commands, malformed shell snippets, zero-test filters, and directory greps as sole proof.
- Security, credentials, generated executable workflow text, destructive operations, and external-service tasks must carry explicit scope boundaries and proof expectations.
- Research or decision tasks must produce concrete artifacts and must not silently authorize implementation before the decision is made.
- If the generated snapshot is not ready for explicit promotion and later parallel execution, write a NO-GO verdict explaining the blocker.

Write `{super_root}/EXECUTION-GATE.md` with:
- `# SUPER EXECUTION GATE`
- A line exactly `Verdict: GO` or `Verdict: NO-GO`
- Queue summary
- Changes made
- Remaining risks
- Promotion and later parallel launch notes

Only write `Verdict: GO` if the generated snapshot is safe and useful for the operator to promote explicitly before a later `auto parallel` run.
"#,
        repo_root = repo_root.display(),
        planning_root = planning_root.display(),
        output_clause = output_clause,
        super_root = super_root.display(),
    )
}

fn require_nonempty_file(path: &Path) -> Result<()> {
    let text =
        fs::read_to_string(path).with_context(|| format!("failed to read {}", path.display()))?;
    if text.trim().is_empty() {
        bail!("{} must not be empty", path.display());
    }
    Ok(())
}

pub(crate) fn audit_generated_plan_against_operator_bans(
    plan_path: &Path,
    operator_prompt: Option<&str>,
) {
    // Best-effort observability: when the operator's prompt enumerates banned
    // path prefixes (typical pattern: "No new docs/ops/...", "No new
    // genesis/checkpoints/0XX-*.md"), count how often the generated plan
    // mentions those prefixes. If many tasks reference banned paths, the
    // operator is likely about to burn cycles producing doc-spam that the
    // AUTO_REJECT_DOCS_ONLY_COMMITS=1 filter will reject downstream. Surface
    // this loudly so the operator can intervene before parallel starts.
    let Some(prompt) = operator_prompt else {
        return;
    };
    let Ok(plan) = std::fs::read_to_string(plan_path) else {
        return;
    };
    let banned_substrings: Vec<&str> = prompt
        .lines()
        .filter(|line| {
            let l = line.to_ascii_lowercase();
            l.contains("no new ") || l.contains("banned") || l.contains("do not create")
        })
        .flat_map(|line| {
            // Extract path-shaped tokens (contain '/' or end with .md).
            line.split(|c: char| c.is_whitespace() || c == ',' || c == '`')
                .filter(|tok| {
                    let t = tok.trim_end_matches(|c: char| !c.is_alphanumeric() && c != '/');
                    !t.is_empty() && (t.contains('/') || t.ends_with(".md")) && !t.starts_with('-')
                })
        })
        .collect();
    if banned_substrings.is_empty() {
        return;
    }
    let mut hits: std::collections::BTreeMap<&str, usize> = std::collections::BTreeMap::new();
    for needle in &banned_substrings {
        let count = plan.matches(*needle).count();
        if count > 0 {
            *hits.entry(*needle).or_insert(0) += count;
        }
    }
    let total: usize = hits.values().sum();
    if total == 0 {
        return;
    }
    eprintln!(
        "warning: generated plan contains {total} mention(s) of operator-banned path prefix(es); \
         AUTO_REJECT_DOCS_ONLY_COMMITS=1 will likely reject commits that touch only these paths. \
         Consider editing IMPLEMENTATION_PLAN.md to remove the banned-pattern tasks before \
         the parallel stage starts."
    );
    let mut entries: Vec<(&&str, &usize)> = hits.iter().collect();
    entries.sort_by(|a, b| b.1.cmp(a.1));
    for (needle, count) in entries.iter().take(8) {
        eprintln!("warning:   {count} hits  {needle}");
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::PathBuf;

    use crate::super_command::EXECUTION_GATE_FILE;

    use super::{build_super_focus, validate_super_execution_gate_report};

    #[test]
    fn build_super_focus_combines_production_directive_and_prompt() {
        let focus = build_super_focus(Some("ship the CLI"), Some("security first"));
        assert!(focus.contains("new CEO"));
        assert!(focus.contains("14 days"));
        assert!(focus.contains("Perfect design/runtime integrity first"));
        assert!(focus.contains("ship the CLI"));
        assert!(focus.contains("security first"));
    }

    #[test]
    fn super_execution_gate_rejects_mixed_verdicts() {
        let gate_path = execution_gate_file(
            "mixed",
            "# SUPER EXECUTION GATE\n\nVerdict: GO\n\nLater:\nVerdict: NO-GO\n",
        );

        let error = validate_super_execution_gate_report(&gate_path)
            .expect_err("mixed verdicts must fail closed");

        assert!(format!("{error:#}").contains("exactly one terminal verdict"));
    }

    #[test]
    fn super_execution_gate_rejects_duplicate_verdicts() {
        let gate_path = execution_gate_file(
            "duplicate",
            "# SUPER EXECUTION GATE\n\nVerdict: GO\n\nQueue summary\n\nVerdict: GO\n",
        );

        let error = validate_super_execution_gate_report(&gate_path)
            .expect_err("duplicate verdicts must fail closed");

        assert!(format!("{error:#}").contains("exactly one terminal verdict"));
    }

    #[test]
    fn super_execution_gate_rejects_missing_verdict() {
        let gate_path = execution_gate_file(
            "missing",
            "# SUPER EXECUTION GATE\n\nQueue summary\n- Not ready\n",
        );

        let error = validate_super_execution_gate_report(&gate_path)
            .expect_err("missing verdicts must fail closed");

        assert!(format!("{error:#}").contains("missing terminal verdict"));
    }

    #[test]
    fn super_execution_gate_rejects_invalid_verdict() {
        let gate_path = execution_gate_file(
            "invalid",
            "# SUPER EXECUTION GATE\n\nVerdict: PASS-ish\n\nQueue summary\n- Ambiguous\n",
        );

        let error = validate_super_execution_gate_report(&gate_path)
            .expect_err("invalid verdicts must fail closed");

        assert!(format!("{error:#}").contains("invalid terminal verdict line"));
    }

    #[test]
    fn super_execution_gate_accepts_single_go_verdict() {
        let gate_path = execution_gate_file(
            "single-go",
            "# SUPER EXECUTION GATE\n\nVerdict: GO\n\nQueue summary\n- Ready\n\nChanges made\n- None\n\nRemaining risks\n- None\n\nPromotion and later parallel launch notes\n- Promote snapshot before launch\n",
        );

        validate_super_execution_gate_report(&gate_path).unwrap();
    }

    fn temp_dir(label: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "autodev-{label}-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&path).unwrap();
        path
    }

    fn execution_gate_file(label: &str, content: &str) -> PathBuf {
        let root = temp_dir(&format!("super-execution-gate-{label}"));
        let super_root = root.join(".auto").join("super").join("run-1");
        fs::create_dir_all(&super_root).unwrap();
        let gate_path = super_root.join(EXECUTION_GATE_FILE);
        fs::write(&gate_path, content).unwrap();
        gate_path
    }
}
