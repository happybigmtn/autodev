//! `auto steward` — stewardship replacement for corpus+gen on mid-flight repos.
//!
//! Both passes run through Codex (gpt-5.4 by default). The first pass reads
//! the repo + planning surface and writes audit artifacts; the second pass is
//! an independent Codex review that verifies the first pass's GHOST / ORPHAN
//! rows against the live tree and applies the approved IMPLEMENTATION_PLAN.md
//! / WORKLIST.md / LEARNINGS.md edits.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};

use crate::codex_exec::run_codex_exec;
use crate::util::{
    atomic_write, auto_checkpoint_if_needed, ensure_repo_layout, git_repo_root, git_stdout,
    push_branch_with_remote_sync, sync_branch_with_remote, timestamp_slug,
};
use crate::StewardArgs;

const STEWARD_DELIVERABLES: [&str; 5] = [
    "DRIFT.md",
    "HINGES.md",
    "RETIRE.md",
    "HAZARDS.md",
    "STEWARDSHIP-REPORT.md",
];

pub(crate) async fn run_steward(args: StewardArgs) -> Result<()> {
    let repo_root = git_repo_root()?;
    ensure_repo_layout(&repo_root)?;
    let current_branch = git_stdout(&repo_root, ["branch", "--show-current"])?
        .trim()
        .to_string();
    if !args.dry_run && !args.report_only && current_branch.is_empty() {
        bail!("auto steward requires a checked-out branch");
    }
    if let Some(required) = args.branch.as_deref() {
        if current_branch != required {
            bail!(
                "auto steward must run on branch `{}` (current: `{}`)",
                required,
                current_branch
            );
        }
    }

    let output_dir = args
        .output_dir
        .clone()
        .unwrap_or_else(|| repo_root.join("steward"));
    fs::create_dir_all(&output_dir)
        .with_context(|| format!("failed to create {}", output_dir.display()))?;
    let reference_repos = resolve_reference_repos(&repo_root, &args.reference_repos)?;
    let planning_surface = detect_planning_surface(&repo_root);

    println!("auto steward");
    println!("repo root:   {}", repo_root.display());
    println!("output dir:  {}", output_dir.display());
    if !current_branch.is_empty() {
        println!("branch:      {}", current_branch);
    }
    println!("steward:     {} ({})", args.model, args.reasoning_effort);
    if args.skip_finalizer {
        println!("finalizer:   (skipped)");
    } else {
        println!(
            "finalizer:   {} ({})",
            args.finalizer_model, args.finalizer_effort
        );
    }
    for path in &reference_repos {
        println!("reference:   {}", path.display());
    }
    println!(
        "planning:    {}",
        planning_surface
            .iter()
            .map(|p| p.display().to_string())
            .collect::<Vec<_>>()
            .join(", ")
    );
    if args.report_only {
        println!("mode:        report-only");
    }

    if !args.dry_run && !args.report_only {
        if let Some(commit) =
            auto_checkpoint_if_needed(&repo_root, current_branch.as_str(), "steward checkpoint")?
        {
            println!("checkpoint:  committed pre-existing changes at {commit}");
        } else if sync_branch_with_remote(&repo_root, current_branch.as_str())? {
            println!("remote sync: rebased onto origin/{}", current_branch);
        }
    }

    let steward_prompt = build_steward_prompt(
        &repo_root,
        &output_dir,
        &current_branch,
        &reference_repos,
        &planning_surface,
        args.report_only,
    );
    let prompt_path = repo_root
        .join(".auto")
        .join("logs")
        .join(format!("steward-{}-prompt.md", timestamp_slug()));
    atomic_write(&prompt_path, steward_prompt.as_bytes())
        .with_context(|| format!("failed to write {}", prompt_path.display()))?;
    println!("prompt log:  {}", prompt_path.display());

    if args.dry_run {
        println!();
        println!("--dry-run: prompt written; model not invoked.");
        return Ok(());
    }

    println!();
    println!("phase:       steward");
    let steward_stderr = output_dir.join("steward.stderr.log");
    let steward_status = run_codex_exec(
        &repo_root,
        &steward_prompt,
        &args.model,
        &args.reasoning_effort,
        &args.codex_bin,
        &steward_stderr,
        None,
        "auto steward",
    )
    .await?;
    if !steward_status.success() {
        bail!(
            "codex steward pass exited with status {}; see {}",
            steward_status
                .code()
                .map(|c: i32| c.to_string())
                .unwrap_or_else(|| "signal".to_string()),
            steward_stderr.display()
        );
    }
    verify_steward_deliverables(&output_dir)?;
    println!(
        "steward:     {} deliverables under {}",
        STEWARD_DELIVERABLES.len(),
        output_dir.display()
    );

    // Post-steward checkpoint so the audit artifacts + any append-only plan
    // edits land even if the finalizer pass fails or is skipped.
    if !args.report_only && !current_branch.is_empty() {
        if let Some(commit) = auto_checkpoint_if_needed(
            &repo_root,
            current_branch.as_str(),
            "steward: audit deliverables",
        )? {
            println!("checkpoint:  committed steward deliverables at {commit}");
        }
    }

    if args.skip_finalizer {
        println!();
        println!("done (finalizer skipped).");
        return Ok(());
    }

    println!();
    println!("phase:       finalizer");
    let finalizer_prompt =
        build_finalizer_prompt(&repo_root, &output_dir, &current_branch, args.report_only);
    let finalizer_prompt_path = repo_root
        .join(".auto")
        .join("logs")
        .join(format!("steward-{}-finalizer-prompt.md", timestamp_slug()));
    atomic_write(&finalizer_prompt_path, finalizer_prompt.as_bytes())
        .with_context(|| format!("failed to write {}", finalizer_prompt_path.display()))?;
    let finalizer_stderr = output_dir.join("finalizer.stderr.log");
    let finalizer_status = run_codex_exec(
        &repo_root,
        &finalizer_prompt,
        &args.finalizer_model,
        &args.finalizer_effort,
        &args.codex_bin,
        &finalizer_stderr,
        None,
        "auto steward finalizer",
    )
    .await?;
    if !finalizer_status.success() {
        bail!(
            "codex finalizer exited with status {}; see {}",
            finalizer_status
                .code()
                .map(|c: i32| c.to_string())
                .unwrap_or_else(|| "signal".to_string()),
            finalizer_stderr.display()
        );
    }

    if !args.report_only && !current_branch.is_empty() {
        if let Some(commit) = auto_checkpoint_if_needed(
            &repo_root,
            current_branch.as_str(),
            "steward: finalizer applied",
        )? {
            println!("checkpoint:  committed finalizer edits at {commit}");
        }
        if push_branch_with_remote_sync(&repo_root, current_branch.as_str())? {
            println!("remote sync: rebased onto origin/{}", current_branch);
        }
    }

    println!();
    println!("auto steward complete");
    println!(
        "report:       {}",
        output_dir.join("STEWARDSHIP-REPORT.md").display()
    );
    println!(
        "final-review: {}",
        output_dir.join("final-review.md").display()
    );
    Ok(())
}

fn resolve_reference_repos(repo_root: &Path, paths: &[PathBuf]) -> Result<Vec<PathBuf>> {
    let mut resolved = Vec::new();
    for path in paths {
        let absolute = if path.is_absolute() {
            path.clone()
        } else {
            repo_root.join(path)
        };
        let canonical = absolute.canonicalize().with_context(|| {
            format!("failed to resolve reference repo {}", absolute.display())
        })?;
        if !canonical.is_dir() {
            bail!("reference repo {} is not a directory", canonical.display());
        }
        if canonical != repo_root {
            resolved.push(canonical);
        }
    }
    resolved.sort();
    resolved.dedup();
    Ok(resolved)
}

fn detect_planning_surface(repo_root: &Path) -> Vec<PathBuf> {
    [
        "IMPLEMENTATION_PLAN.md",
        "REVIEW.md",
        "SECURITY_PLAN.md",
        "WORKLIST.md",
        "LEARNINGS.md",
        "ARCHIVED.md",
        "AGENTS.md",
        "CLAUDE.md",
        "PLANS.md",
    ]
    .iter()
    .map(|name| repo_root.join(name))
    .filter(|p| p.exists())
    .collect()
}

fn verify_steward_deliverables(output_dir: &Path) -> Result<()> {
    let mut missing = Vec::new();
    for name in STEWARD_DELIVERABLES {
        let path = output_dir.join(name);
        if !path.exists() || fs::metadata(&path).map(|m| m.len()).unwrap_or(0) == 0 {
            missing.push(name);
        }
    }
    if !missing.is_empty() {
        bail!(
            "steward deliverables missing or empty in {}: {}",
            output_dir.display(),
            missing.join(", ")
        );
    }
    Ok(())
}

fn build_steward_prompt(
    repo_root: &Path,
    output_dir: &Path,
    branch: &str,
    reference_repos: &[PathBuf],
    planning_surface: &[PathBuf],
    report_only: bool,
) -> String {
    let steward_dir = output_dir
        .strip_prefix(repo_root)
        .unwrap_or(output_dir)
        .display()
        .to_string();
    let reference_clause = if reference_repos.is_empty() {
        String::new()
    } else {
        let listing = reference_repos
            .iter()
            .map(|path| format!("- `{}`", path.display()))
            .collect::<Vec<_>>()
            .join("\n");
        format!(
            "\n## Cross-repo references\n\n{listing}\n\nFor every shared interface \
             (wire format, admin envelope, chain-tx type, FROST / key-material \
             reference, cross-repo bridge), check both sides implement the same \
             contract. Flag divergences in `DRIFT.md` under a `Cross-repo contracts` \
             section.\n"
        )
    };
    let planning_listing = if planning_surface.is_empty() {
        "- (none detected — this repo may be greenfield; consider `auto corpus` instead)"
            .to_string()
    } else {
        planning_surface
            .iter()
            .map(|path| {
                let rel = path
                    .strip_prefix(repo_root)
                    .unwrap_or(path)
                    .display()
                    .to_string();
                format!("- `{}`", rel)
            })
            .collect::<Vec<_>>()
            .join("\n")
    };
    let apply_clause = if report_only {
        "You are in REPORT-ONLY mode. Produce the five deliverables below but do \
         NOT edit `IMPLEMENTATION_PLAN.md`, `WORKLIST.md`, `LEARNINGS.md`, or any \
         other planning file. Put proposed edits inside `STEWARDSHIP-REPORT.md` \
         under a `Proposed plan edits` section for the Codex finalizer to apply."
    } else {
        "After writing the five deliverables, make the following in-place edits \
         to the active planning surface:\n\n\
         - `IMPLEMENTATION_PLAN.md`: append new tasks for every high-confidence \
           GHOST or drift finding. Use the repo's existing item-id convention \
           (`W2-NS-*`, `NEM-*`, `BIT-NS-*`, `P-*`, etc.) and match the shape of \
           surrounding items (Owns / Scope / Acceptance / Verification / \
           Required tests / Dependencies / Estimated scope).\n\
         - `WORKLIST.md`: append severity-tagged findings for medium-confidence \
           issues that need reviewer attention but not a full plan entry.\n\
         - `LEARNINGS.md`: append durable lessons observed from drift patterns.\n\n\
         Do NOT edit `REVIEW.md`, `ARCHIVED.md`, or code. Do NOT delete or \
         rewrite existing items — append only. The finalizer pass reviews your \
         edits and trims or flags them."
    };
    format!(
        r#"You are the **steward** of this repository at `{repo_root}`, branch `{branch}`.

A steward is not a re-planner. This repo already has an active planning surface and a running auto-loop. Your job is to:

1. Reconcile what the planning surface claims against what the live tree actually shows.
2. Identify the 3-5 items whose completion would most collapse remaining scope.
3. Name the things that should be retired (dead code, stale fixtures, superseded specs).
4. Map the security vs feature hazard grid so mainnet-blocking work is unambiguous.
5. Apply the approved updates directly to the planning surface so the findings land.

## Active planning surface

{planning_listing}
{reference_clause}

## Deliverables

Write all five files into `{steward_dir}/`. Do NOT write `genesis/`, numbered ExecPlans, or any competing master plan — that is `auto corpus`'s job and this repo already has its own active planning.

### 1. `{steward_dir}/DRIFT.md`

For every tracked item id in the planning surface (W2-NS-*, NEM-*, BIT-NS-*, P-*, V68-*, V70-W-*, B-OBS-*, OLYMPIAD-*, TUI-*, etc.), produce one row:

| item | claim | reality | verdict | evidence |
|---|---|---|---|---|
| `W2-NS-V03` | SHIPPED decision layer, binary pending | decision layer at `crates/bridge-signer/src/daemon.rs:122`; binary `crates/bridge-signer-bin/` does not exist | DRIFT | `git ls-files crates/bridge-signer-bin 2>/dev/null` empty |
| `P-020B` | app-server NL backend live | cited files `observatory-tui/src/nl/*.rs` deleted by commit `6ad9b7632` | GHOST | `git log --all --diff-filter=D -- observatory-tui/src/nl/app_server.rs` returns `6ad9b7632` |

Verdicts: `AGREES` / `DRIFT` (partial) / `GHOST` (claim with no code) / `ORPHAN` (code without plan). Keep evidence column verifiable with a short grep or git command. Group rows by arc so the operator can scan.

### 2. `{steward_dir}/HINGES.md`

Three to five items whose completion would most collapse remaining backlog. For each:
- Item id + one-line hypothesis for why it is a hinge.
- Follow-on items that become trivial or obsolete once it lands.
- Evidence (cited files, blockers, partial work).

A hinge removes N-1 degrees of freedom from the plan by landing. Be strict.

### 3. `{steward_dir}/RETIRE.md`

Explicit retirement candidates. For each:
- Path or id
- Why: superseded by X, dead code since commit Y, unreferenced fixture, duplicated content.
- Confidence: `HIGH` (safe to delete) / `MEDIUM` (needs sign-off) / `LOW` (needs investigation).

### 4. `{steward_dir}/HAZARDS.md`

2x2 grid (mainnet-blocking × user-facing) for every open item. Exhaustive — no ellipses.

```
|                | user-facing          | not user-facing     |
|----------------|----------------------|---------------------|
| mainnet-block  | (items)              | (items)             |
| not mainnet-b  | (items)              | (items)             |
```

### 5. `{steward_dir}/STEWARDSHIP-REPORT.md`

Executive summary, 2-3 pages max:
- `## Verdict` — is this repo's active plan trustworthy? biggest risk?
- `## Top three drift findings` — the worst rows from DRIFT.md.
- `## Hinges ranked` — ordered.
- `## Retirement batch` — summary of RETIRE.md.
- `## Mainnet launch blockers` — the mainnet-blocking column of HAZARDS.md.
- `## Proposed plan edits` — ordered list of explicit append-only edits (file + exact markdown + rationale). Finalizer reads this.
- `## Decision log` — Taste or User Challenge calls classified.

## How to work

- Verify before claiming. Every row must cite a file or git command.
- Batch reads with parallel subagents when useful.
- Do not invent items. If the queue is messy, say so; do not paper over it.
- Stay scoped. This is reconciliation, not a re-plan.

## Plan-edit policy

{apply_clause}

Each deliverable <= 20 KB. Tables > prose. Skip preamble.
"#,
        repo_root = repo_root.display(),
        branch = if branch.is_empty() { "(detached)" } else { branch },
        planning_listing = planning_listing,
        reference_clause = reference_clause,
        steward_dir = steward_dir,
        apply_clause = apply_clause,
    )
}

fn build_finalizer_prompt(
    repo_root: &Path,
    output_dir: &Path,
    branch: &str,
    report_only: bool,
) -> String {
    let steward_dir = output_dir
        .strip_prefix(repo_root)
        .unwrap_or(output_dir)
        .display()
        .to_string();
    let apply_clause = if report_only {
        "The first pass ran in report-only mode; you are also in report-only \
         mode. Review the deliverables for plausibility, write your verdict, \
         and stop. Do NOT edit any planning surface."
    } else {
        "The first Codex pass has either (a) written its proposed edits \
         directly into `IMPLEMENTATION_PLAN.md` / `WORKLIST.md` / `LEARNINGS.md`, \
         or (b) staged them inside the `## Proposed plan edits` section of \
         `STEWARDSHIP-REPORT.md`. Your job: verify and reconcile:\n\n\
         - Read the cited files in the repo to confirm each claim holds.\n\
         - If the first pass wrote directly into a planning surface and the \
           claim holds, leave it. If the entry shape does not match the \
           surrounding file's convention, fix it or remove it.\n\
         - If the first pass staged edits in `STEWARDSHIP-REPORT.md` and they \
           hold, apply them in-place to the planning surface.\n\
         - Never delete or rewrite existing plan items during this pass; only \
           append or trim.\n\n\
         Write your verdict to `steward/final-review.md` with sections \
         `## Accepted`, `## Rejected`, `## Deferred`, `## Plan-surface diff \
         summary`. End with a `PASS | CONCERNS | FAIL` verdict.\n\n\
         Commit any edits with message `steward: finalizer applied`."
    };
    format!(
        r#"You are the finalizer for an `auto steward` pass. Both passes run under Codex gpt-5.4 by default; you are the second, independent look.

The first pass produced five artifacts. Verify they hold in the live tree and apply the approved IMPLEMENTATION_PLAN.md / WORKLIST.md / LEARNINGS.md edits.

## Inputs

- `{steward_dir}/DRIFT.md`
- `{steward_dir}/HINGES.md`
- `{steward_dir}/RETIRE.md`
- `{steward_dir}/HAZARDS.md`
- `{steward_dir}/STEWARDSHIP-REPORT.md`
- Planning surface at repo root (`IMPLEMENTATION_PLAN.md`, `REVIEW.md`, `SECURITY_PLAN.md`, `WORKLIST.md`, `LEARNINGS.md` if present).
- Branch: `{branch}`

## What to verify

1. Sample every `GHOST` row from DRIFT.md: confirm the cited files really do not exist.
2. Sample every `ORPHAN` row: confirm the code path really exists without a plan entry.
3. For every entry in HINGES.md, confirm the "follow-on items that become obsolete" claim by looking at the named items.
4. For the mainnet-blocking column of HAZARDS.md, confirm every item is actually in `SECURITY_PLAN.md` under a launch-gate annotation.

## Plan-edit policy

{apply_clause}

## Hard rules

- Do NOT rewrite the first pass's deliverable files. Your verdict goes in `steward/final-review.md`.
- Do NOT invent new items. If you think the first pass missed something, record it under `## Finalizer addenda` in `final-review.md` as a follow-up.
- Stay on branch `{branch}`.
"#,
        steward_dir = steward_dir,
        branch = if branch.is_empty() { "(detached)" } else { branch },
        apply_clause = apply_clause,
    )
}

#[cfg(test)]
mod tests {
    use super::{build_finalizer_prompt, build_steward_prompt, detect_planning_surface};
    use std::fs;
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn unique_temp_dir() -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        std::env::temp_dir().join(format!("auto-steward-test-{nanos}"))
    }

    #[test]
    fn detect_planning_surface_only_lists_files_that_exist() {
        let temp = unique_temp_dir();
        fs::create_dir_all(&temp).expect("create temp");
        fs::write(temp.join("IMPLEMENTATION_PLAN.md"), b"plan").expect("write plan");
        fs::write(temp.join("REVIEW.md"), b"review").expect("write review");
        let surface = detect_planning_surface(&temp);
        let rels: Vec<String> = surface
            .iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().to_string())
            .collect();
        assert!(rels.contains(&"IMPLEMENTATION_PLAN.md".to_string()));
        assert!(rels.contains(&"REVIEW.md".to_string()));
        assert!(!rels.contains(&"SECURITY_PLAN.md".to_string()));
        fs::remove_dir_all(temp).expect("cleanup");
    }

    #[test]
    fn steward_prompt_includes_report_only_instructions_when_flagged() {
        let temp = unique_temp_dir();
        fs::create_dir_all(&temp).expect("create temp");
        let prompt = build_steward_prompt(
            &temp,
            &temp.join("steward"),
            "trunk",
            &[],
            &[temp.join("IMPLEMENTATION_PLAN.md")],
            true,
        );
        assert!(prompt.contains("REPORT-ONLY"));
        assert!(prompt.contains("do NOT edit"));
        assert!(!prompt.contains("append new tasks for every high-confidence"));
        fs::remove_dir_all(temp).expect("cleanup");
    }

    #[test]
    fn steward_prompt_instructs_in_place_edits_by_default() {
        let temp = unique_temp_dir();
        fs::create_dir_all(&temp).expect("create temp");
        let prompt = build_steward_prompt(
            &temp,
            &temp.join("steward"),
            "trunk",
            &[],
            &[temp.join("IMPLEMENTATION_PLAN.md")],
            false,
        );
        assert!(prompt.contains("append new tasks for every high-confidence"));
        assert!(prompt.contains("WORKLIST.md"));
        assert!(prompt.contains("LEARNINGS.md"));
        assert!(!prompt.contains("REPORT-ONLY"));
        fs::remove_dir_all(temp).expect("cleanup");
    }

    #[test]
    fn steward_prompt_emits_cross_repo_section_when_references_present() {
        let temp = unique_temp_dir();
        fs::create_dir_all(&temp).expect("create temp");
        let other = unique_temp_dir();
        fs::create_dir_all(&other).expect("create other");
        let prompt = build_steward_prompt(
            &temp,
            &temp.join("steward"),
            "trunk",
            std::slice::from_ref(&other),
            &[temp.join("IMPLEMENTATION_PLAN.md")],
            false,
        );
        assert!(prompt.contains("Cross-repo references"));
        assert!(prompt.contains(&other.display().to_string()));
        fs::remove_dir_all(temp).expect("cleanup");
        fs::remove_dir_all(other).expect("cleanup other");
    }

    #[test]
    fn finalizer_prompt_forbids_rewriting_steward_artifacts() {
        let temp = unique_temp_dir();
        fs::create_dir_all(&temp).expect("create temp");
        let prompt = build_finalizer_prompt(&temp, &temp.join("steward"), "trunk", false);
        assert!(prompt.contains("Do NOT rewrite"));
        assert!(prompt.contains("final-review.md"));
        assert!(prompt.contains("Accepted"));
        fs::remove_dir_all(temp).expect("cleanup");
    }
}
