use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use chrono::Local;

use crate::codex_exec::run_codex_exec_max_context;
use crate::task_parser::{
    parse_task_header, parse_tasks, task_field_body_until_any, TaskStatus, TASK_FIELD_BOUNDARIES,
};
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
- Keep fields in the exact order listed below so shared parsers stop each field at the same boundary.
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
- `Dependencies:` is scheduler input, not prose. It must be exactly `none` or only comma-separated/backticked task IDs already present in `{plan_path}` (for example ``Dependencies: `TASK-001`, `TASK-002` `` or one `- `TASK-ID`` per line). Do not include parentheticals, wave notes, "parallel with", "after", "blocked by", "depends on", or explanatory text in this field.
- `Estimated scope:` must be `XS`, `S`, or `M`; split larger work.
- `Verification:` and `Required tests:` must contain scoped executable commands or explicit non-executable proof. Do not let metadata fields appear inside them.
- `Completion artifacts:` must be `none` or concrete repo-relative proof/artifact paths.
- Every new task must be parseable by the same shared task parser used by `auto parallel`; do not rely on prose-only gates, compact follow-on rows, or markdown tables.

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
    if !text.contains(&spec_ref) && !text.contains(&absolute_spec_ref) {
        bail!(
            "auto spec plan output {} must reference {}",
            plan_path.display(),
            spec_path.display()
        );
    }
    let tasks = parse_tasks(&text);
    let all_task_ids = tasks
        .iter()
        .map(|task| task.id.as_str())
        .collect::<std::collections::BTreeSet<_>>();
    let spec_tasks = tasks
        .iter()
        .filter(|task| {
            task.markdown.contains(&spec_ref) || task.markdown.contains(&absolute_spec_ref)
        })
        .collect::<Vec<_>>();
    if spec_tasks.is_empty() {
        bail!(
            "auto spec plan output {} references {} but no parseable task owns that reference",
            plan_path.display(),
            spec_path.display()
        );
    }
    for task in spec_tasks {
        verify_auto_spec_plan_task(
            task,
            &all_task_ids,
            plan_path,
            spec_path,
            &spec_ref,
            &absolute_spec_ref,
        )?;
    }
    Ok(())
}

fn verify_auto_spec_plan_task(
    task: &crate::task_parser::PlanTask,
    all_task_ids: &std::collections::BTreeSet<&str>,
    plan_path: &Path,
    spec_path: &Path,
    spec_ref: &str,
    absolute_spec_ref: &str,
) -> Result<()> {
    let header = task.markdown.lines().next().unwrap_or_default();
    if !header.starts_with("- [ ] `") {
        bail!(
            "auto spec task `{}` in {} must use canonical unchecked header `- [ ] `TASK-ID` Title`",
            task.id,
            plan_path.display()
        );
    }
    let (status, header_id, title) = parse_task_header(header)
        .with_context(|| format!("auto spec task `{}` header did not parse", task.id))?;
    if status != TaskStatus::Pending || header_id != task.id || title.trim().is_empty() {
        bail!(
            "auto spec task `{}` in {} must be pending and have a non-empty title",
            task.id,
            plan_path.display()
        );
    }

    for field in PLAN_REQUIRED_FIELDS {
        let body = task_field_body(task, field)?;
        if body.trim().is_empty() {
            bail!(
                "auto spec task `{}` in {} has empty required field `{field}`",
                task.id,
                plan_path.display()
            );
        }
    }

    let spec_value = first_field_line(task, "Spec:")?;
    if !spec_value.contains(spec_ref) && !spec_value.contains(absolute_spec_ref) {
        bail!(
            "auto spec task `{}` `Spec:` field must point at {}; got `{spec_value}`",
            task.id,
            spec_path.display()
        );
    }

    verify_scheduler_dependencies(task, all_task_ids)?;
    verify_estimated_scope(task)?;
    verify_completion_artifacts(task)?;
    verify_field_did_not_swallow_metadata(task, "Verification:")?;
    verify_field_did_not_swallow_metadata(task, "Required tests:")?;
    Ok(())
}

fn task_field_body(task: &crate::task_parser::PlanTask, field: &str) -> Result<String> {
    task_field_body_until_any(&task.markdown, field, TASK_FIELD_BOUNDARIES)
        .with_context(|| format!("task `{}` missing `{field}`", task.id))
}

fn first_field_line(task: &crate::task_parser::PlanTask, field: &str) -> Result<String> {
    let body = task_field_body(task, field)?;
    body.lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .map(str::to_string)
        .with_context(|| format!("task `{}` has no value for `{field}`", task.id))
}

fn verify_scheduler_dependencies(
    task: &crate::task_parser::PlanTask,
    all_task_ids: &std::collections::BTreeSet<&str>,
) -> Result<()> {
    let body = task_field_body(task, "Dependencies:")?;
    let meaningful_lines = body
        .lines()
        .map(strip_plan_bullet)
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>();
    if meaningful_lines.is_empty() {
        bail!("auto spec task `{}` has empty `Dependencies:`", task.id);
    }

    let joined = meaningful_lines.join(" ");
    if joined.eq_ignore_ascii_case("none") {
        if !task.dependencies.is_empty() {
            bail!(
                "auto spec task `{}` says `Dependencies: none` but parser found {:?}",
                task.id,
                task.dependencies
            );
        }
        return Ok(());
    }
    reject_dependency_prose(task, &joined)?;

    let mut explicit = Vec::new();
    for line in meaningful_lines {
        for part in line.split(',') {
            let token = part.trim();
            if token.is_empty() {
                continue;
            }
            let Some(unwrapped) = token
                .strip_prefix('`')
                .and_then(|rest| rest.strip_suffix('`'))
            else {
                bail!(
                    "auto spec task `{}` `Dependencies:` must contain only backticked task IDs or `none`; got `{token}`",
                    task.id
                );
            };
            explicit.push(unwrapped.to_string());
        }
    }
    explicit.sort();
    explicit.dedup();
    let mut parsed = task.dependencies.clone();
    parsed.sort();
    if explicit != parsed {
        bail!(
            "auto spec task `{}` dependencies are not parser-stable; explicit {:?}, parsed {:?}",
            task.id,
            explicit,
            parsed
        );
    }
    for dependency in &task.dependencies {
        if dependency == &task.id {
            bail!("auto spec task `{}` cannot depend on itself", task.id);
        }
        if !all_task_ids.contains(dependency.as_str()) {
            bail!(
                "auto spec task `{}` depends on `{dependency}`, which is not a parseable task in the plan",
                task.id
            );
        }
    }
    Ok(())
}

fn reject_dependency_prose(task: &crate::task_parser::PlanTask, text: &str) -> Result<()> {
    let lower = text.to_ascii_lowercase();
    for phrase in [
        "parallel", "wave", "after ", "once ", "blocked", "gated", "depends", "external", "(", ")",
        ".", ";", ":",
    ] {
        if lower.contains(phrase) {
            bail!(
                "auto spec task `{}` `Dependencies:` must be machine-readable IDs only; remove prose phrase `{phrase}`",
                task.id
            );
        }
    }
    Ok(())
}

fn verify_estimated_scope(task: &crate::task_parser::PlanTask) -> Result<()> {
    let scope = first_field_line(task, "Estimated scope:")?;
    if !matches!(scope.as_str(), "XS" | "S" | "M") {
        bail!(
            "auto spec task `{}` must use `Estimated scope: XS`, `S`, or `M`; got `{scope}`",
            task.id
        );
    }
    Ok(())
}

fn verify_completion_artifacts(task: &crate::task_parser::PlanTask) -> Result<()> {
    let body = task_field_body(task, "Completion artifacts:")?;
    let first = body
        .lines()
        .map(strip_plan_bullet)
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or_default()
        .to_ascii_lowercase();
    if first == "none" {
        return Ok(());
    }
    if task.completion_artifacts.is_empty() {
        bail!(
            "auto spec task `{}` `Completion artifacts:` must be `none` or concrete repo-relative paths",
            task.id
        );
    }
    Ok(())
}

fn verify_field_did_not_swallow_metadata(
    task: &crate::task_parser::PlanTask,
    field: &str,
) -> Result<()> {
    let body = task_field_body(task, field)?;
    for boundary in TASK_FIELD_BOUNDARIES
        .iter()
        .filter(|boundary| **boundary != field)
    {
        if body
            .lines()
            .map(strip_plan_bullet)
            .map(str::trim)
            .any(|line| line.starts_with(boundary))
        {
            bail!(
                "auto spec task `{}` `{field}` body swallowed metadata boundary `{boundary}`",
                task.id
            );
        }
    }
    Ok(())
}

fn strip_plan_bullet(line: &str) -> &str {
    let trimmed = line.trim_start();
    for bullet in ["- ", "* ", "+ "] {
        if let Some(rest) = trimmed.strip_prefix(bullet) {
            return rest;
        }
    }
    trimmed
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

#[cfg(test)]
mod tests {
    use super::verify_plan_output;
    use std::fs;
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_root(name: &str) -> PathBuf {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        let root = std::env::temp_dir().join(format!("autodev-spec-{name}-{unique}"));
        fs::create_dir_all(root.join("specs")).expect("create temp specs");
        root
    }

    fn valid_plan(spec_ref: &str, dependency_line: &str) -> String {
        format!(
            r#"# IMPLEMENTATION_PLAN

## Priority Work

- [ ] `SPEC-001` Runtime foundation

    Spec: `{spec_ref}`
    Why now: downstream UI depends on runtime truth.
    Codebase evidence: `src/runtime.rs` owns the current fact model.
    Source of truth: `src/runtime.rs`
    Runtime owner: `src/runtime.rs`
    UI consumers: `web/src/App.tsx`
    Generated artifacts: none
    Fixture boundary: production code cannot import fixture/demo/sample data.
    Retired surfaces: none
    Owns: `src/runtime.rs`
    Integration touchpoints: `web/src/App.tsx`
    Scope boundary: runtime contract only.
    Acceptance criteria: API returns canonical facts without UI fallback truth.
    Verification: `cargo test -p app runtime_foundation`
    Required tests: `cargo test -p app runtime_foundation`
    Contract generation: none -- no generated contract
    Cross-surface tests: `npm test -- runtime-readback`
    Review/closeout: reviewer checks runtime-to-UI readback proof.
    Completion artifacts: `docs/proof/runtime-foundation.md`
    Dependencies: none
    Estimated scope: S
    Completion signal: proof recorded and tests pass.

- [ ] `SPEC-002` UI readback

    Spec: `{spec_ref}`
    Why now: UI must render runtime-owned facts.
    Codebase evidence: `web/src/App.tsx` currently renders the surface.
    Source of truth: `src/runtime.rs`
    Runtime owner: `src/runtime.rs`
    UI consumers: `web/src/App.tsx`
    Generated artifacts: none
    Fixture boundary: production code cannot import fixture/demo/sample data.
    Retired surfaces: none
    Owns: `web/src/App.tsx`
    Integration touchpoints: `src/runtime.rs`
    Scope boundary: UI readback only.
    Acceptance criteria: UI displays runtime payload without local catalogs.
    Verification: `npm test -- runtime-readback`
    Required tests: `npm test -- runtime-readback`
    Contract generation: none -- no generated contract
    Cross-surface tests: `npm test -- runtime-readback`
    Review/closeout: reviewer checks no duplicated truth in UI.
    Completion artifacts: `docs/proof/ui-readback.md`
    Dependencies: {dependency_line}
    Estimated scope: M
    Completion signal: proof recorded and tests pass.
"#
        )
    }

    #[test]
    fn auto_spec_plan_validation_accepts_parallel_ready_tasks() {
        let root = temp_root("valid");
        let plan_path = root.join("IMPLEMENTATION_PLAN.md");
        let spec_path = root.join("specs/300426-runtime-ui.md");
        fs::write(
            &plan_path,
            valid_plan("specs/300426-runtime-ui.md", "`SPEC-001`"),
        )
        .expect("write plan");

        verify_plan_output(&plan_path, &spec_path).expect("parallel-ready plan validates");
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn auto_spec_plan_validation_rejects_prose_dependencies() {
        let root = temp_root("dependency-prose");
        let plan_path = root.join("IMPLEMENTATION_PLAN.md");
        let spec_path = root.join("specs/300426-runtime-ui.md");
        fs::write(
            &plan_path,
            valid_plan(
                "specs/300426-runtime-ui.md",
                "`SPEC-001` (parallel with `SPEC-999`)",
            ),
        )
        .expect("write plan");

        let error = verify_plan_output(&plan_path, &spec_path).expect_err("prose rejected");
        assert!(error
            .to_string()
            .contains("Dependencies:` must be machine-readable IDs only"));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn auto_spec_plan_validation_requires_canonical_headers() {
        let root = temp_root("canonical-header");
        let plan_path = root.join("IMPLEMENTATION_PLAN.md");
        let spec_path = root.join("specs/300426-runtime-ui.md");
        let plan = valid_plan("specs/300426-runtime-ui.md", "`SPEC-001`").replace(
            "- [ ] `SPEC-002` UI readback",
            "- [ ] SPEC-002 - UI readback",
        );
        fs::write(&plan_path, plan).expect("write plan");

        let error = verify_plan_output(&plan_path, &spec_path).expect_err("header rejected");
        assert!(error
            .to_string()
            .contains("must use canonical unchecked header"));
        let _ = fs::remove_dir_all(root);
    }
}
