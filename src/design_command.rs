use std::fs;
use std::path::Path;

use anyhow::{bail, Context, Result};
use serde::Serialize;

use crate::codex_exec::run_codex_exec_max_context;
use crate::util::{
    atomic_write, binary_provenance_line, ensure_repo_layout, git_repo_root, timestamp_slug,
};
use crate::{DesignArgs, SuperArgs};

const DESIGN_ARTIFACTS: [&str; 6] = [
    "DESIGN-AUDIT.md",
    "DESIGN-SYSTEM-PROPOSAL.md",
    "ENGINE-UI-CONTRACT.md",
    "FRONTEND-QA.md",
    "DESIGN-PLAN-ITEMS.md",
    "DESIGN-REPORT.md",
];

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
    skip_qa: bool,
    binary: String,
}

pub(crate) async fn run_design(args: DesignArgs) -> Result<()> {
    let repo_root = git_repo_root()?;
    ensure_repo_layout(&repo_root)?;
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
        return Ok(());
    }

    run_design_codex_phase(
        &repo_root,
        &output_dir,
        &prompt,
        &args.model,
        &args.reasoning_effort,
        &args.codex_bin,
        "auto-design",
    )
    .await?;
    verify_design_artifacts(&output_dir)?;
    println!("status:      design artifacts verified");
    Ok(())
}

pub(crate) async fn run_super_design_module(
    args: &SuperArgs,
    repo_root: &Path,
    planning_root: &Path,
    super_root: &Path,
) -> Result<()> {
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
    Super,
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
            DesignRunKind::Super => {
                "- You may amend root `DESIGN.md` and the planning corpus design files so `auto gen` inherits the design contract. Do not edit source code, root specs, or root `IMPLEMENTATION_PLAN.md` in this pre-generation super module."
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
        DesignRunKind::Super => {
            "You are the `auto super` design perfection gate running after corpus and before generation. Design is first-class and blocking: do not subordinate, soften, or defer design/runtime integrity findings into a later generic review."
        }
    };

    format!(
        r#"{stage_clause}

Repository: `{repo_root}`
{planning_clause}
Output directory: `{output_dir}`
{prompt_clause}

Your job is to synthesize expert design review, design-system consultation, web interface guidelines, frontend design craft, and QA into a repo-native design contract. This is not a fake mockup generator. This is a design/runtime integrity pass that must be perfected before broader functional lanes proceed.

Use these lenses together:
- Plan design review: rate and close gaps in information architecture, interaction states, journey, AI-slop risk, design-system alignment, responsive behavior, accessibility, and unresolved design decisions.
- Design consultation: infer or improve a coherent product-specific system: aesthetic direction, safe category conventions, deliberate creative risks, typography, color, spacing, layout, motion, and component vocabulary.
- Web interface guidelines: fetch or recall current web UI/a11y best practices and apply them to actual frontend files, not generic screenshots.
- Frontend design craft: avoid generic AI aesthetics, overused fonts, purple-gradient defaults, meaningless cards, generic dashboard widgets, and product-copy fog. Existing design tokens and component patterns outrank generic advice.
- QA discipline: test what a real user can do, check console/runtime errors after interactions, verify responsive states, and capture evidence or exact blockers.
- Additional skills.sh design synthesis: use product-frontend critique for message clarity, frontend-ui-ux engineering for accessible polish and micro-interactions, and design-token extraction discipline from design-system skills. Do not require external paid design tools or infinite-canvas mockup systems.

Required first reads:
- `AGENTS.md` or repo-local agent instructions.
- Product doctrine: `README.md`, `DESIGN.md`, GDD/OS/invariant docs when present.
- Planning truth: `IMPLEMENTATION_PLAN.md`, `REVIEW.md`, active `specs/`, and `{planning_root_display}` when present.
- Frontend code: app/routes/components/styles/design tokens/tests/build scripts.
- Runtime/engine/API code that owns facts displayed by UI.
- Generated bindings/schemas/client code and their regeneration commands when present.

Hard rules:
- Do not create fake mockups as acceptance evidence. Preview pages are allowed only as proposals and must be labeled non-authoritative.
- Do not invent frontend bindings, constants, catalogs, balances, settlement math, eligibility rules, risk classes, or status derivations. UI must consume runtime/API/generated truth.
- If the design calls for new data, name the runtime owner, API/schema change, generator, consumer, and test/readback proof.
- Prefer existing helpers, generated clients, hooks, stores, route loaders, and design tokens over new manual glue.
- Production code must not import fixture/demo/sample data as fallback truth.
- Retired or superseded screens/specs must be deleted, archived, tombstoned, or explicitly blocked from active implementation.
- A design improvement is not complete unless it names the engine/API contract and the proof that would fail if UI drifts again.

{edit_clause}
{qa_clause}

Write these non-empty artifacts under `{output_dir}`:
1. `DESIGN-AUDIT.md`
   - Current UI/design-system inventory.
   - Existing frontend design signals and reusable components/tokens.
   - 0-10 ratings for the seven plan-design-review dimensions.
   - AI-slop risks and modern/stunning UI opportunities specific to this product.
2. `DESIGN-SYSTEM-PROPOSAL.md`
   - Proposed or revised `DESIGN.md` doctrine.
   - Aesthetic thesis, safe choices, deliberate risks, typography, color, spacing, layout, motion, components, empty/error/loading states, responsive and accessibility rules.
   - Explicitly explain what belongs in real product UI versus non-authoritative concept previews.
3. `ENGINE-UI-CONTRACT.md`
   - Table of UI surfaces, runtime/API source of truth, existing helpers/bindings, generated artifacts, fixture boundary, and required drift guard.
   - Call out every manual binding or duplicated frontend derivation found.
4. `FRONTEND-QA.md`
   - Commands/URLs/tools used, screenshots or artifact paths if produced, console/runtime findings, responsive findings, and exact blockers.
   - Separate confirmed breaks from hypotheses and from skipped/unavailable checks.
5. `DESIGN-PLAN-ITEMS.md`
   - Queue-ready plan items for unresolved design/runtime gaps using the repo's implementation-plan field style.
   - Every item must include runtime owner, UI consumers, generated artifacts, contract generation, cross-surface proof, and closeout review.
6. `DESIGN-REPORT.md`
   - Executive summary, files changed if any, recommended next workflow step, and GO/NO-GO for design-aware implementation.
   - In the `auto super` flow, `Verdict: NO-GO` blocks the CEO production campaign until design/runtime integrity is repaired.

If `{apply_status}`:
- Update `DESIGN.md` only with durable doctrine grounded in the live product and existing frontend.
- In standalone mode, add or amend plan/spec items only for real unresolved work. In super mode, prefer amending the planning corpus so `auto gen` emits the queue.
- Do not mark any implementation item complete.

Final line of `DESIGN-REPORT.md` must be exactly one of:
- `Verdict: GO`
- `Verdict: NO-GO`
"#,
        stage_clause = stage_clause,
        repo_root = repo_root.display(),
        planning_clause = planning_clause,
        output_dir = output_dir.display(),
        prompt_clause = prompt_clause,
        planning_root_display = planning_root
            .map(|path| path.display().to_string())
            .unwrap_or_else(|| "no planning corpus".to_string()),
        edit_clause = edit_clause,
        qa_clause = qa_clause,
        apply_status = if apply {
            "applying edits is enabled"
        } else {
            "report-only mode is enabled"
        },
    )
}

fn verify_design_artifacts(output_dir: &Path) -> Result<()> {
    for artifact in DESIGN_ARTIFACTS {
        require_nonempty_file(&output_dir.join(artifact))?;
    }
    let report_path = output_dir.join("DESIGN-REPORT.md");
    let report = fs::read_to_string(&report_path)
        .with_context(|| format!("failed to read {}", report_path.display()))?;
    if !report
        .lines()
        .any(|line| matches!(line.trim(), "Verdict: GO" | "Verdict: NO-GO"))
    {
        bail!(
            "{} must contain `Verdict: GO` or `Verdict: NO-GO`",
            report_path.display()
        );
    }
    Ok(())
}

fn require_design_go(output_dir: &Path) -> Result<()> {
    let report_path = output_dir.join("DESIGN-REPORT.md");
    let report = fs::read_to_string(&report_path)
        .with_context(|| format!("failed to read {}", report_path.display()))?;
    if !report.lines().any(|line| line.trim() == "Verdict: GO") {
        bail!(
            "design perfection gate did not approve downstream generation; expected `Verdict: GO` in {}",
            report_path.display()
        );
    }
    Ok(())
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
    use super::{build_design_prompt, DesignRunKind};
    use std::path::PathBuf;

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
}
