use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use chrono::Local;

use crate::claude_exec::run_claude_exec;
use crate::codex_exec::run_codex_exec_max_context;
use crate::task_parser::{
    execution_row_first_field_line, parse_task_header, parse_tasks, validate_execution_row,
    TaskStatus,
};
use crate::util::{atomic_write, ensure_repo_layout, git_repo_root, timestamp_slug};
use crate::SpecArgs;

const SPEC_REQUIRED_SECTIONS: [&str; 13] = [
    "## Objective",
    "## Product Experience Contract",
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
    let stdout_log_path = prompt_path.with_file_name(
        prompt_path
            .file_name()
            .and_then(|value| value.to_str())
            .unwrap_or("spec-prompt.md")
            .replace("-prompt.md", "-stdout.log"),
    );
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
    println!(
        "context:    {}",
        if spec_author_uses_claude_model(&args.model) {
            "provider maximum"
        } else {
            "max"
        }
    );
    println!("prompt log: {}", prompt_path.display());
    println!("stdout log: {}", stdout_log_path.display());
    if args.dry_run {
        println!("\n{full_prompt}");
        return Ok(());
    }

    let status = if spec_author_uses_claude_model(&args.model) {
        run_claude_exec(
            &repo_root,
            &full_prompt,
            &args.model,
            &args.reasoning_effort,
            Some(args.max_turns),
            &stderr_log_path,
            Some(&stdout_log_path),
            "auto-spec",
        )
        .await?
    } else {
        run_codex_exec_max_context(
            &repo_root,
            &full_prompt,
            &args.model,
            &args.reasoning_effort,
            &args.codex_bin,
            &stderr_log_path,
            Some(&stdout_log_path),
            "auto-spec",
        )
        .await?
    };
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

fn spec_author_uses_claude_model(model: &str) -> bool {
    let normalized = model.trim().to_ascii_lowercase();
    normalized.is_empty()
        || normalized == "opus"
        || normalized == "sonnet"
        || normalized == "haiku"
        || normalized.contains("claude")
        || normalized.contains("opus")
        || normalized.contains("sonnet")
        || normalized.contains("haiku")
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
  - `## Product Experience Contract`
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
- `## Product Experience Contract` is mandatory and must come before source inventory. It is the product-manager/designer gate:
  - If the request affects a UI, TUI, CLI user surface, browser route, report, operator workflow, or viewer experience, start this section with actual surface design, not prose about process.
  - Name the user promise, the first read, the second read, the third read, and why that hierarchy matters.
  - Include concrete surface plates/mockups. For TUI work, include breakpoint plates such as `80x24`, `120x32`, and `160x48` when relevant. For browser/app work, include desktop and mobile first-viewport plates. For report/CLI/operator workflows, include the exact command/status/output surface a user sees.
  - Include a state storyboard for empty/loading/degraded/live/error/success states that the implementation must render.
  - Describe visual hierarchy and copy budget with Bloomberg-terminal-level specificity: what occupies the dominant region, what sits in rails/tapes/drawers, what is intentionally omitted, and what proof/source labels must remain visible.
  - If there is truly no user-facing surface, write exactly `none -- no user-facing surface` and do not pad it with boilerplate.
- `## Source Of Truth` must name runtime owner modules/APIs, UI consumers, generated artifacts, and retired/superseded surfaces. Use `none` only after checking. This section must not appear before the product experience contract.
- `## Evidence Status` must separate verified code facts, recommendations, hypotheses, and unresolved questions.
- `## Runtime Contract` must state which engine/runtime/API owns canonical facts and what must fail closed when data is missing.
- `## UI Contract` must state how UI consumes runtime truth without duplicating catalogs, constants, settlement math, eligibility rules, risk classifications, or sample fallback truth.
- `## Generated Artifacts` must name bindings/schemas/docs/snapshots to regenerate, or `none`.
- `## Fixture Policy` must quarantine sample/demo/test data away from production runtime components.
- `## Retired / Superseded Surfaces` must name old specs/files/contracts that must not be implemented from, or `none`.
- `## Acceptance Criteria` must be concrete observable bullets.
- UI/product acceptance criteria must describe observable surfaces: named screen/route/pane, first-read hierarchy, required state variants, breakpoint/screenshot/readback proof, and rejected anti-patterns. Do not use criteria like "improve polish", "make beautiful", or "add tests" without saying what must be seen.
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
  - `Lane kind:`
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
- `Lane kind:` must be exactly `code`, `operator`, or `evidence`.
- `Dependencies:` is scheduler input, not prose. It must be exactly `none` or only comma-separated/backticked task IDs already present in `{plan_path}` (for example ``Dependencies: `TASK-001`, `TASK-002` `` or one `- `TASK-ID`` per line). Do not include parentheticals, wave notes, "parallel with", "after", "blocked by", "depends on", or explanatory text in this field.
- `Estimated scope:` must be `XS`, `S`, or `M`; split larger work.
- `Verification:` and `Required tests:` must contain scoped executable commands or explicit non-executable proof. Do not let metadata fields appear inside them.
- `Completion artifacts:` must be `none` or concrete repo-relative proof/artifact paths.
- Every new task must be parseable by the same shared task parser used by `auto parallel`; do not rely on prose-only gates, compact follow-on rows, or markdown tables.

Process rules to encode in the spec and task split:
- Product/user value comes before artifact production. A spec is not ready if it starts as a source inventory, test list, schema list, or implementation decomposition before saying what the user will actually experience.
- Runtime owns facts; UI renders facts.
- Implement runtime/engine/API changes before UI changes.
- Regenerate contracts before adapting consumers.
- Fixture/sample/demo data belongs only in tests, story/demo harnesses, or explicit dev-only paths.
- For UI changes, include at least one runtime-output-to-UI-readback acceptance path.
- Retire/delete/tombstone superseded surfaces as first-class work, not optional cleanup.
- A task is not done until the original requirement cannot reappear without a guard, test, grep assertion, or review check failing.
- For product/UI work, at least one first task must lock the actual design plates and the later implementation tasks must cite those plates. Do not ask engineers to infer layout, visual hierarchy, copy, or state design from generic acceptance criteria.
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
    verify_product_experience_contract(&text, spec_path)?;
    Ok(())
}

fn verify_product_experience_contract(markdown: &str, spec_path: &Path) -> Result<()> {
    let body = section_body(markdown, "## Product Experience Contract").unwrap_or_default();
    let normalized = body.trim().to_ascii_lowercase();
    if normalized == "none -- no user-facing surface" {
        return Ok(());
    }

    let required: [(&str, &[&str]); 3] = [
        (
            "surface plate",
            &[
                "surface plate",
                "surface plates",
                "mockup",
                "mockups",
                "wireframe",
                "wireframes",
                "viewport",
                "plate --",
                "plate -",
                "plate \u{2014}",
                "surface 1",
                "surface 2",
                "exact command",
                "exact output",
                "json-rpc",
                "\"jsonrpc\"",
                "http/1.1",
                "80x24",
                "120x32",
                "160x48",
            ],
        ),
        (
            "visual hierarchy",
            &["visual hierarchy", "first read", "second read"],
        ),
        (
            "state storyboard",
            &["state storyboard", "empty", "degraded", "live"],
        ),
    ];
    for (label, needles) in required {
        if !needles.iter().any(|needle| normalized.contains(needle)) {
            bail!(
                "auto spec output {} has a Product Experience Contract but lacks {label} detail; include actual plates/mockups, visual hierarchy, and state storyboard or write exactly `none -- no user-facing surface`",
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

    validate_execution_row(task, all_task_ids).with_context(|| {
        format!(
            "auto spec task `{}` failed execution-row validation",
            task.id
        )
    })?;

    let spec_value = execution_row_first_field_line(task, "Spec:")?;
    if !spec_value.contains(spec_ref) && !spec_value.contains(absolute_spec_ref) {
        bail!(
            "auto spec task `{}` `Spec:` field must point at {}; got `{spec_value}`",
            task.id,
            spec_path.display()
        );
    }
    Ok(())
}

fn section_has_body(markdown: &str, header: &str) -> bool {
    section_body(markdown, header).is_some_and(|body| !body.trim().is_empty())
}

fn section_body<'a>(markdown: &'a str, header: &str) -> Option<&'a str> {
    let start = markdown.find(header)?;
    let body_start = start + header.len();
    let after = &markdown[body_start..];
    let body_end = after
        .find("\n## ")
        .map(|offset| body_start + offset)
        .unwrap_or(markdown.len());
    Some(&markdown[body_start..body_end])
}

#[cfg(test)]
mod tests {
    use super::{spec_author_uses_claude_model, verify_plan_output, verify_spec_output};
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
    Cross-surface tests: `cargo test -p app runtime_readback`
    Review/closeout: reviewer checks runtime-to-UI readback proof.
    Completion artifacts: `docs/proof/runtime-foundation.md`
    Lane kind: code
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
    Verification: `cargo test -p app runtime_readback`
    Required tests: `cargo test -p app runtime_readback`
    Contract generation: none -- no generated contract
    Cross-surface tests: `cargo test -p app runtime_readback`
    Review/closeout: reviewer checks no duplicated truth in UI.
    Completion artifacts: `docs/proof/ui-readback.md`
    Lane kind: code
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
        let message = format!("{error:#}");
        assert!(
            message.contains("Dependencies:` must be machine-readable IDs only"),
            "{message}"
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn auto_spec_rejects_prose_dependency_fields() {
        let root = temp_root("dependency-prose-alias");
        let plan_path = root.join("IMPLEMENTATION_PLAN.md");
        let spec_path = root.join("specs/300426-runtime-ui.md");
        fs::write(
            &plan_path,
            valid_plan(
                "specs/300426-runtime-ui.md",
                "`SPEC-001` after runtime owner confirms the API",
            ),
        )
        .expect("write plan");

        let error = verify_plan_output(&plan_path, &spec_path).expect_err("prose rejected");
        let message = format!("{error:#}");
        assert!(
            message.contains("Dependencies:` must be machine-readable IDs only"),
            "{message}"
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn auto_spec_plan_validation_rejects_multi_filter_verification_commands() {
        let root = temp_root("multi-filter-verification");
        let plan_path = root.join("IMPLEMENTATION_PLAN.md");
        let spec_path = root.join("specs/300426-runtime-ui.md");
        let plan = valid_plan("specs/300426-runtime-ui.md", "`SPEC-001`").replace(
            "cargo test -p app runtime_foundation",
            "cargo test generation::tests::one completion_artifacts::tests::two",
        );
        fs::write(&plan_path, plan).expect("write plan");

        let error = verify_plan_output(&plan_path, &spec_path).expect_err("verification rejected");
        let message = format!("{error:#}");
        assert!(message.contains("multi-filter cargo test"), "{message}");
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn auto_spec_plan_validation_rejects_malformed_grep_verification_commands() {
        let root = temp_root("malformed-grep-verification");
        let plan_path = root.join("IMPLEMENTATION_PLAN.md");
        let spec_path = root.join("specs/300426-runtime-ui.md");
        let plan = valid_plan("specs/300426-runtime-ui.md", "`SPEC-001`").replace(
            "cargo test -p app runtime_foundation",
            "grep -n Runtime src",
        );
        fs::write(&plan_path, plan).expect("write plan");

        let error = verify_plan_output(&plan_path, &spec_path).expect_err("verification rejected");
        let message = format!("{error:#}");
        assert!(message.contains("malformed grep verification"), "{message}");
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

    #[test]
    fn auto_spec_routes_opus_defaults_to_claude_author() {
        assert!(spec_author_uses_claude_model("opus"));
        assert!(spec_author_uses_claude_model("claude-opus-4-7"));
        assert!(!spec_author_uses_claude_model("gpt-5.5"));
    }

    #[test]
    fn auto_spec_requires_product_experience_contract() {
        let root = temp_root("missing-product-contract");
        let spec_path = root.join("specs/300426-ui.md");
        fs::write(
            &spec_path,
            r#"# Specification: UI polish

## Objective
Improve the UI.

## Source Of Truth
`src/ui.rs`

## Evidence Status
verified: current UI exists.

## Runtime Contract
none

## UI Contract
The UI renders the state.

## Generated Artifacts
none

## Fixture Policy
none

## Retired / Superseded Surfaces
none

## Acceptance Criteria
- UI improves.

## Verification
`cargo test`

## Review And Closeout
reviewer checks it.

## Open Questions
none
"#,
        )
        .expect("write spec");

        let error = verify_spec_output(&spec_path).expect_err("missing section rejected");
        assert!(error.to_string().contains("Product Experience Contract"));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn auto_spec_rejects_ui_contract_without_actual_design_plates() {
        let root = temp_root("weak-product-contract");
        let spec_path = root.join("specs/300426-ui.md");
        fs::write(
            &spec_path,
            r#"# Specification: UI polish

## Objective
Improve the UI.

## Product Experience Contract
Users need a better dashboard with cleaner visual polish and stronger hierarchy.

## Source Of Truth
`src/ui.rs`

## Evidence Status
verified: current UI exists.

## Runtime Contract
none

## UI Contract
The UI renders the state.

## Generated Artifacts
none

## Fixture Policy
none

## Retired / Superseded Surfaces
none

## Acceptance Criteria
- UI improves.

## Verification
`cargo test`

## Review And Closeout
reviewer checks it.

## Open Questions
none
"#,
        )
        .expect("write spec");

        let error = verify_spec_output(&spec_path).expect_err("weak design rejected");
        assert!(error.to_string().contains("lacks surface plate detail"));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn auto_spec_accepts_developer_facing_text_plates() {
        let root = temp_root("text-plate-product-contract");
        let spec_path = root.join("specs/300426-mcp.md");
        fs::write(
            &spec_path,
            r#"# Specification: MCP catalog

## Objective
Make the catalog easier to maintain without changing public tools.

## Product Experience Contract
There is no browser or TUI. The product surface is the JSON-RPC payload and generated reference markdown.

### Surface 1 - `tools/list` JSON-RPC response
The agent's first read is the `tools` array. The second read is each tool name and schema. The third read is the per-property schema detail.

Plate - Admin profile:
```
{ "jsonrpc": "2.0", "result": { "tools": [ { "name": "player_session_start" } ] } }
```

Visual hierarchy: the tool name dominates, then the description, then the schema block.

State storyboard:
- empty: no tools are advertised.
- degraded: refused calls return a profile-specific error.
- live: listed tools dispatch to handlers.
- success: reference markdown regenerates byte-identical.

## Source Of Truth
`src/mcp.rs`

## Evidence Status
verified: current catalog exists.

## Runtime Contract
`src/mcp.rs` owns dispatch.

## UI Contract
JSON-RPC clients consume runtime-owned tool definitions.

## Generated Artifacts
reference markdown

## Fixture Policy
production code cannot import fixture/demo/sample data.

## Retired / Superseded Surfaces
none

## Acceptance Criteria
- Tools remain byte-identical.

## Verification
`cargo test mcp_catalog`

## Review And Closeout
reviewer compares the generated reference.

## Open Questions
none
"#,
        )
        .expect("write spec");

        verify_spec_output(&spec_path).expect("text plates validate");
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn auto_spec_accepts_exact_command_status_output_surfaces() {
        let root = temp_root("command-surface-product-contract");
        let spec_path = root.join("specs/300426-cli.md");
        fs::write(
            &spec_path,
            r#"# Specification: CLI status

## Objective
Make status output easier to trust.

## Product Experience Contract
The user-facing surface is terminal output.

Exact command/status/output surface:
```
$ auto status
status: degraded
next: run auto doctor
```

First read: `status: degraded`. Second read: the `next:` action. Third read: detailed blocker rows below the summary.

Visual hierarchy: one status line first, one next action second, detailed rows last.

State storyboard:
- empty: no queue rows are present.
- degraded: blockers are listed with next actions.
- live: active lanes show pid and task id.
- success: status is green and next action is explicit.

## Source Of Truth
`src/status.rs`

## Evidence Status
verified: command exists.

## Runtime Contract
`src/status.rs` owns queue facts.

## UI Contract
terminal output renders queue facts without inventing status.

## Generated Artifacts
none

## Fixture Policy
production code cannot import fixture/demo/sample data.

## Retired / Superseded Surfaces
none

## Acceptance Criteria
- Status output includes one explicit next action.

## Verification
`cargo test status_output`

## Review And Closeout
reviewer compares fixture stdout.

## Open Questions
none
"#,
        )
        .expect("write spec");

        verify_spec_output(&spec_path).expect("command surfaces validate");
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn auto_spec_accepts_none_product_experience_for_non_surface_work() {
        let root = temp_root("none-product-contract");
        let spec_path = root.join("specs/300426-runtime.md");
        fs::write(
            &spec_path,
            r#"# Specification: Runtime cleanup

## Objective
Clean the runtime path.

## Product Experience Contract
none -- no user-facing surface

## Source Of Truth
`src/runtime.rs`

## Evidence Status
verified: runtime path exists.

## Runtime Contract
`src/runtime.rs` owns the invariant.

## UI Contract
none

## Generated Artifacts
none

## Fixture Policy
production code cannot import fixture/demo/sample data.

## Retired / Superseded Surfaces
none

## Acceptance Criteria
- Runtime invariant is enforced.

## Verification
`cargo test runtime_cleanup`

## Review And Closeout
reviewer checks the invariant.

## Open Questions
none
"#,
        )
        .expect("write spec");

        verify_spec_output(&spec_path).expect("non-surface spec validates");
        let _ = fs::remove_dir_all(root);
    }
}
