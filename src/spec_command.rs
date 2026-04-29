use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use chrono::Local;

use crate::codex_exec::run_codex_exec_max_context;
use crate::util::{atomic_write, ensure_repo_layout, git_repo_root, timestamp_slug};
use crate::SpecArgs;

const SPEC_REQUIRED_SECTIONS: [&str; 12] = [
    "## Objective",
    "## Source Of Truth",
    "## Evidence Status",
    "## Runtime Contract",
    "## UI Contract",
    "## Generated Artifacts",
    "## Fixture Policy",
    "## Retired / Superseded Surfaces",
    "## Acceptance Criteria",
    "## Verification",
    "## Review And Closeout",
    "## Open Questions",
];

const PLAN_REQUIRED_FIELDS: [&str; 22] = [
    "Spec:",
    "Why now:",
    "Codebase evidence:",
    "Source of truth:",
    "Runtime owner:",
    "UI consumers:",
    "Generated artifacts:",
    "Fixture boundary:",
    "Retired surfaces:",
    "Owns:",
    "Integration touchpoints:",
    "Scope boundary:",
    "Acceptance criteria:",
    "Verification:",
    "Required tests:",
    "Contract generation:",
    "Cross-surface tests:",
    "Review/closeout:",
    "Completion artifacts:",
    "Dependencies:",
    "Estimated scope:",
    "Completion signal:",
];

pub(crate) async fn run_spec(args: SpecArgs) -> Result<()> {
    let repo_root = git_repo_root()?;
    ensure_repo_layout(&repo_root)?;
    let prompt = args
        .prompt
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .context("auto spec requires a prompt, e.g. `auto spec \"sync UI with runtime\"`")?;
    let spec_path = args
        .spec_path
        .clone()
        .unwrap_or_else(|| repo_root.join("specs").join(default_spec_filename(prompt)));
    let plan_path = args
        .plan_path
        .clone()
        .unwrap_or_else(|| repo_root.join("IMPLEMENTATION_PLAN.md"));
    let spec_path = absolutize(&repo_root, &spec_path);
    let plan_path = absolutize(&repo_root, &plan_path);
    let log_root = repo_root.join(".auto").join("spec");
    fs::create_dir_all(&log_root)
        .with_context(|| format!("failed to create {}", log_root.display()))?;
    let prompt_path = log_root.join(format!("spec-{}-prompt.md", timestamp_slug()));
    let stderr_log_path = prompt_path.with_file_name(
        prompt_path
            .file_name()
            .and_then(|value| value.to_str())
            .unwrap_or("spec-prompt.md")
            .replace("-prompt.md", "-stderr.log"),
    );
    let full_prompt = build_spec_prompt(&repo_root, prompt, &spec_path, &plan_path);
    atomic_write(&prompt_path, full_prompt.as_bytes())
        .with_context(|| format!("failed to write {}", prompt_path.display()))?;

    println!("auto spec");
    println!("repo root:  {}", repo_root.display());
    println!("spec path:  {}", spec_path.display());
    println!("plan path:  {}", plan_path.display());
    println!("model:      {}", args.model);
    println!("effort:     {}", args.reasoning_effort);
    println!("prompt log: {}", prompt_path.display());
    if args.dry_run {
        println!("\n{full_prompt}");
        return Ok(());
    }

    let status = run_codex_exec_max_context(
        &repo_root,
        &full_prompt,
        &args.model,
        &args.reasoning_effort,
        &args.codex_bin,
        &stderr_log_path,
        None,
        "auto-spec",
    )
    .await?;
    if !status.success() {
        bail!(
            "auto spec authoring failed with status {status}; see {}",
            stderr_log_path.display()
        );
    }
    verify_spec_output(&spec_path)?;
    verify_plan_output(&plan_path, &spec_path)?;
    println!("status:     spec and plan items verified");
    Ok(())
}

fn absolutize(repo_root: &Path, path: &Path) -> PathBuf {
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        repo_root.join(path)
    }
}

fn default_spec_filename(prompt: &str) -> String {
    let date = Local::now().format("%d%m%y");
    let mut slug = prompt
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() {
                ch.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect::<String>();
    while slug.contains("--") {
        slug = slug.replace("--", "-");
    }
    let slug = slug.trim_matches('-');
    let slug = if slug.is_empty() {
        "generated-spec"
    } else {
        slug
    };
    let slug = slug.chars().take(56).collect::<String>();
    format!("{date}-{slug}.md")
}

fn build_spec_prompt(repo_root: &Path, prompt: &str, spec_path: &Path, plan_path: &Path) -> String {
    format!(
        r#"You are running `auto spec` for the repository at `{repo_root}`.

Operator request:
{prompt}

Write exactly these repo files:
- Spec: `{spec_path}`
- Plan items: append or insert into `{plan_path}`

Do not print the spec to stdout. Edit the files directly.

First inspect repository truth:
- Read `AGENTS.md` or the repo's agent instructions.
- Read the current implementation plan and active specs relevant to the request.
- Read source code before writing current-state claims. Docs are claims, not truth.

Spec contract for `{spec_path}`:
- First line must be `# Specification: <short title>`.
- Include these exact non-empty sections:
  - `## Objective`
  - `## Source Of Truth`
  - `## Evidence Status`
  - `## Runtime Contract`
  - `## UI Contract`
  - `## Generated Artifacts`
  - `## Fixture Policy`
  - `## Retired / Superseded Surfaces`
  - `## Acceptance Criteria`
  - `## Verification`
  - `## Review And Closeout`
  - `## Open Questions`
- `## Source Of Truth` must name runtime owner modules/APIs, UI consumers, generated artifacts, and retired/superseded surfaces. Use `none` only after checking.
- `## Evidence Status` must separate verified code facts, recommendations, hypotheses, and unresolved questions.
- `## Runtime Contract` must state which engine/runtime/API owns canonical facts and what must fail closed when data is missing.
- `## UI Contract` must state how UI consumes runtime truth without duplicating catalogs, constants, settlement math, eligibility rules, risk classifications, or sample fallback truth.
- `## Generated Artifacts` must name bindings/schemas/docs/snapshots to regenerate, or `none`.
- `## Fixture Policy` must quarantine sample/demo/test data away from production runtime components.
- `## Retired / Superseded Surfaces` must name old specs/files/contracts that must not be implemented from, or `none`.
- `## Acceptance Criteria` must be concrete observable bullets.
- `## Verification` must list narrow commands or runtime checks.
- `## Review And Closeout` must say how `auto review` or a human reviewer independently verifies each plan item, including grep/assertion proof where simple tests are insufficient.

Plan item contract for `{plan_path}`:
- Add dependency-ordered unchecked items under `## Priority Work` or `## Follow-On Work`.
- Preserve existing unfinished items and completed history.
- Each item header MUST be exactly: `` - [ ] `<TASK-ID>` <Title> `` (task ID wrapped in backticks). The task ID must start with an uppercase letter, contain at least one digit, contain at least one hyphen, and use only `[A-Za-z0-9-]` characters.
- Insert a blank line between the header and the first field line.
- Field lines are 4-space-indented `<Field>: <value>` plain lines, NOT markdown bullets. Do not prefix field names with `- `.
- Every new unfinished item must include these exact fields:
  - `Spec:`
  - `Why now:`
  - `Codebase evidence:`
  - `Source of truth:`
  - `Runtime owner:`
  - `UI consumers:`
  - `Generated artifacts:`
  - `Fixture boundary:`
  - `Retired surfaces:`
  - `Owns:`
  - `Integration touchpoints:`
  - `Scope boundary:`
  - `Acceptance criteria:`
  - `Verification:`
  - `Required tests:`
  - `Contract generation:`
  - `Cross-surface tests:`
  - `Review/closeout:`
  - `Completion artifacts:`
  - `Dependencies:`
  - `Estimated scope:`
  - `Completion signal:`
- `Source of truth:` must name the canonical runtime/API/doc owner.
- `Runtime owner:` names the engine/runtime path or `none`.
- `UI consumers:` names concrete UI paths/routes or `none`.
- `Generated artifacts:` names bindings/schemas/docs to regenerate or `none`.
- `Fixture boundary:` states production cannot import fixture/demo/sample data, or says why not applicable.
- `Retired surfaces:` names stale specs/files/contracts to delete/archive/tombstone or `none`.
- `Contract generation:` names the generation/check command or `none -- no generated contract`.
- `Cross-surface tests:` names a runtime-to-UI/readback proof when UI is affected, or `none -- no UI/runtime boundary`.
- `Review/closeout:` must describe independent proof for the original requirement, not just `cargo check`.
- `Estimated scope:` must be `XS`, `S`, or `M`; split larger work.

Process rules to encode in the spec and task split:
- Runtime owns facts; UI renders facts.
- Implement runtime/engine/API changes before UI changes.
- Regenerate contracts before adapting consumers.
- Fixture/sample/demo data belongs only in tests, story/demo harnesses, or explicit dev-only paths.
- For UI changes, include at least one runtime-output-to-UI-readback acceptance path.
- Retire/delete/tombstone superseded surfaces as first-class work, not optional cleanup.
- A task is not done until the original requirement cannot reappear without a guard, test, grep assertion, or review check failing.
"#,
        repo_root = repo_root.display(),
        prompt = prompt,
        spec_path = spec_path.display(),
        plan_path = plan_path.display(),
    )
}

fn verify_spec_output(spec_path: &Path) -> Result<()> {
    let text = fs::read_to_string(spec_path)
        .with_context(|| format!("auto spec did not write {}", spec_path.display()))?;
    if !text.starts_with("# Specification:") {
        bail!(
            "auto spec output {} must start with `# Specification:`",
            spec_path.display()
        );
    }
    for section in SPEC_REQUIRED_SECTIONS {
        if !section_has_body(&text, section) {
            bail!(
                "auto spec output {} is missing non-empty `{section}`",
                spec_path.display()
            );
        }
    }
    Ok(())
}

fn verify_plan_output(plan_path: &Path, spec_path: &Path) -> Result<()> {
    let text = fs::read_to_string(plan_path)
        .with_context(|| format!("auto spec did not update {}", plan_path.display()))?;
    let spec_ref = spec_path
        .strip_prefix(plan_path.parent().unwrap_or_else(|| Path::new(".")))
        .unwrap_or(spec_path)
        .display()
        .to_string();
    let absolute_spec_ref = spec_path.display().to_string();
    let Some(spec_ref_index) = text
        .find(&spec_ref)
        .or_else(|| text.find(&absolute_spec_ref))
    else {
        bail!(
            "auto spec plan output {} must reference {}",
            plan_path.display(),
            spec_path.display()
        );
    };
    let plan_item = plan_item_around(&text, spec_ref_index);
    for field in PLAN_REQUIRED_FIELDS {
        if !plan_item_contains_field(plan_item, field) {
            bail!(
                "auto spec plan item for {} is missing required field `{field}`",
                plan_path.display()
            );
        }
    }
    Ok(())
}

fn plan_item_around(markdown: &str, index: usize) -> &str {
    let start = markdown[..index]
        .rfind("\n- [")
        .map(|offset| offset + 1)
        .unwrap_or(0);
    let end = markdown[index..]
        .find("\n- [")
        .map(|offset| index + offset)
        .unwrap_or(markdown.len());
    &markdown[start..end]
}

fn plan_item_contains_field(markdown: &str, field: &str) -> bool {
    if markdown.contains(field) {
        return true;
    }
    let without_colon = field.trim_end_matches(':');
    let bold_field = format!("**{without_colon}:**");
    markdown.contains(&bold_field)
}

fn section_has_body(markdown: &str, header: &str) -> bool {
    let Some(start) = markdown.find(header) else {
        return false;
    };
    let body_start = start + header.len();
    let after = &markdown[body_start..];
    let body_end = after
        .find("\n## ")
        .map(|offset| body_start + offset)
        .unwrap_or(markdown.len());
    !markdown[body_start..body_end].trim().is_empty()
}
