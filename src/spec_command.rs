use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use chrono::Local;

use crate::codex_exec::run_codex_exec_max_context;
use crate::task_parser::{
    execution_row_first_field_line, parse_task_header, parse_tasks, validate_execution_row,
    TaskStatus,
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
    "## Autonomous Defaults",
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
  - `## Autonomous Defaults`
- `## Source Of Truth` must name runtime owner modules/APIs, UI consumers, generated artifacts, and retired/superseded surfaces. Use `none` only after checking.
- `## Evidence Status` must separate verified code facts, recommendations, hypotheses, and external blockers. Do not leave routine product/engineering choices as open questions when a conservative default can be chosen.
- `## Runtime Contract` must state which engine/runtime/API owns canonical facts and what must fail closed when data is missing.
- `## UI Contract` must state how UI consumes runtime truth without duplicating catalogs, constants, settlement math, eligibility rules, risk classifications, or sample fallback truth.
- Keep metadata sections concise and path-specific. Put real implementation substance in `## Runtime Contract`, `## UI Contract`, `## Acceptance Criteria`, and `## Verification`, not in generic artifact/process boilerplate.
- `## Generated Artifacts` is a generated-output inventory. Name generated bindings, schemas, ABI files, generated reports, snapshots, generated docs, or build metadata that require a regeneration/check command. Write `none` when the surface only creates authored docs, checkpoints, screenshots, ordinary tests, or source files.
- `## Autonomous Defaults` must resolve questions using the best conservative default the agent can justify from code, docs, primary-source references, and repo direction. Use this section for chosen defaults and external blockers; do not emit unresolved question lists.
- For app/product work, cover the end-to-end production path affected by the request: launch/configuration, auth or identity, security hardening, transaction/API smoke proof, visual regression or UX proof, observability, rollback/recovery, and first-run DX. Cite existing proof when already complete; add plan items when not.
- For production app/system work, include generalized production-grade controls where applicable: existing reusable assets, explicit non-scope, data-flow/control-flow proof, global release gates, test coverage map, critical failure modes and recovery paths, and a worktree/parallelization strategy that gives independent agents disjoint ownership.
- For generated interfaces or generated reports/snapshots, require a generated surface manifest, a regeneration/freshness command, and CI or local verification that fails on stale generated artifacts. This applies to generated clients, schemas, wrappers, SDK bindings, ABI/interface descriptors, build manifests, and generated proof reports in any stack.
- For value-moving, security-sensitive, or externally integrated behavior, specify source/authentication validation, replay/idempotency handling, rollback/compensation or failure policy, resource/performance/cost budget snapshots, and adversarial/property/mutation/fault-injection coverage where the repo has a practical harness.
- For user-facing UI, require state and accessibility proof for loading, empty, error, success, partial/stale, pending/locked/rejected, reduced-motion, responsive target breakpoints, keyboard/screen-reader/contrast behavior, and non-color-only cues where applicable.
- Do not create specs whose main substance is boilerplate about artifacts or reviews. A production spec should make runtime behavior, UI behavior, integration behavior, or verification obligations precise enough for `auto parallel` to implement without guessing.
- `## Fixture Policy` must quarantine sample/demo/test data away from production runtime components.
- `## Retired / Superseded Surfaces` must name old specs/files/contracts that must not be implemented from, or `none`.
- `## Acceptance Criteria` must be concrete observable bullets.
- `## Verification` must list narrow commands or runtime checks.
- `## Review And Closeout` must say how an autonomous agent independently verifies each plan item, including executable commands, grep/assertion proof, generated artifacts, or browser/test evidence where simple tests are insufficient.

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
- `Generated artifacts:` is only for generated outputs that require a generation/check command, such as bindings, schemas, ABI files, generated reports, snapshots, generated docs, or build metadata. Do not list ordinary authored docs, checkpoint markdown, screenshots captured by a worker, source files, or tests here; put those in `Completion artifacts:` instead. Use `Generated artifacts: none -- no generated artifact` when no generated output is owned by the task.
- `Fixture boundary:` states production cannot import fixture/demo/sample data, or says why not applicable.
- `Retired surfaces:` names stale specs/files/contracts to delete/archive/tombstone or `none`.
- `Contract generation:` names the generation/check command for affected generated artifacts, or `none -- no generated contract` when `Generated artifacts:` is none.
- `Cross-surface tests:` names a runtime-to-UI/readback proof when UI is affected, or `none -- no UI/runtime boundary`.
- `Review/closeout:` must describe independent proof for the original requirement, not just `cargo check`.
- The first `Acceptance criteria:` clause must name a production behavior, runtime contract, operator-visible result, or executable proof artifact. Do not use generic process completion as the first criterion.
- `Lane kind:` must be exactly one of `code`, `operator`, or `evidence`. Use `code` for source/runtime/frontend implementation, `evidence` for docs/checkpoints/research/verification-only work, and `operator` for deploy, secrets, live infrastructure, credentials, or external operations that still have an executable runbook or explicit `AUTO_ENV_BLOCKER`.
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
- Do not leave human-only review or approval gates. Convert them into executable checks, autonomous closeout criteria, or explicit `AUTO_ENV_BLOCKER`/external-operation prerequisites when credentials, live infrastructure, or legal/account access is genuinely unavailable to the agent.
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
    if !spec_acceptance_criteria_has_bullet(&text) {
        bail!(
            "auto spec output {} must include `## Acceptance Criteria` with at least one bullet",
            spec_path.display()
        );
    }
    lint_spec_shape(spec_path, &text)?;
    Ok(())
}

fn spec_acceptance_criteria_has_bullet(markdown: &str) -> bool {
    section_body(markdown, "## Acceptance Criteria")
        .map(|body| {
            body.lines().any(|line| {
                let trimmed = line.trim_start();
                trimmed.starts_with("- ") || trimmed.starts_with("* ")
            })
        })
        .unwrap_or(false)
}

fn lint_spec_shape(spec_path: &Path, markdown: &str) -> Result<()> {
    if markdown.contains("## Open Questions") {
        bail!(
            "auto spec output {} must use `## Autonomous Defaults`, not `## Open Questions`",
            spec_path.display()
        );
    }
    lint_spec_autonomous_defaults(
        spec_path,
        section_body(markdown, "## Autonomous Defaults").unwrap_or_default(),
    )?;
    lint_spec_generated_artifacts(
        spec_path,
        section_body(markdown, "## Generated Artifacts").unwrap_or_default(),
    )?;
    Ok(())
}

fn lint_spec_autonomous_defaults(spec_path: &Path, section_body: &str) -> Result<()> {
    let body = section_body.trim();
    if body.is_empty() {
        bail!(
            "auto spec output {} `## Autonomous Defaults` must choose defaults or name external blockers",
            spec_path.display()
        );
    }
    let lower = body.to_ascii_lowercase();
    let weak_body = lower
        .trim_matches(|ch: char| ch.is_whitespace() || ch == '-' || ch == '*' || ch == '.')
        .trim();
    if weak_body == "none" || weak_body == "n/a" || weak_body == "tbd" {
        bail!(
            "auto spec output {} `## Autonomous Defaults` cannot be only `{weak_body}`",
            spec_path.display()
        );
    }
    if body.contains('?')
        || [
            "open question",
            "unresolved question",
            "to be decided",
            "tbd",
            "todo",
            "ask the user",
            "needs human",
            "human review",
        ]
        .iter()
        .any(|needle| lower.contains(needle))
    {
        bail!(
            "auto spec output {} `## Autonomous Defaults` must not contain unresolved-question or human-blocker language",
            spec_path.display()
        );
    }
    Ok(())
}

fn lint_spec_generated_artifacts(spec_path: &Path, section_body: &str) -> Result<()> {
    let lower = section_body.to_ascii_lowercase();
    if lower.trim().is_empty() || spec_field_value_like_none(&lower) {
        return Ok(());
    }
    let authored_markers = [
        "docs/",
        "review.md",
        "readme.md",
        "changelog.md",
        "app/src/",
        "src/",
        "tests/",
        "scripts/",
        "screenshot",
        "playwright",
        "checkpoint",
        "receipt",
        "runbook",
        "handoff",
    ];
    let generated_markers = [
        "generated",
        "/gen/",
        "gen/",
        "build/",
        "dist/",
        "wrapper",
        "schema",
        "abi",
        "snapshot",
        "manifest",
        "report",
        "coverage",
        "baseline",
        "mutation",
        "fuzz",
        "perf",
        "cost",
        "resource",
        "interface",
        "artifacts/",
        "openapi",
        "protobuf",
    ];
    if authored_markers.iter().any(|needle| lower.contains(needle))
        && !generated_markers
            .iter()
            .any(|needle| lower.contains(needle))
    {
        bail!(
            "auto spec output {} `## Generated Artifacts` appears to list authored proof/source artifacts; reserve it for generated outputs",
            spec_path.display()
        );
    }
    Ok(())
}

fn spec_field_value_like_none(value: &str) -> bool {
    let trimmed = value
        .trim()
        .trim_matches(|ch: char| ch.is_whitespace() || ch == '-' || ch == '*' || ch == '.')
        .trim();
    trimmed == "none" || trimmed.starts_with("none --")
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

    validate_execution_row(task, all_task_ids).map_err(|err| {
        anyhow::anyhow!(
            "auto spec task `{}` failed execution-row validation: {err:#}",
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
    section_body(markdown, header)
        .map(|body| !body.trim().is_empty())
        .unwrap_or(false)
}

fn section_body<'a>(markdown: &'a str, header: &str) -> Option<&'a str> {
    let Some(start) = markdown.find(header) else {
        return None;
    };
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
    use super::{verify_plan_output, verify_spec_output};
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

    fn valid_spec() -> &'static str {
        r#"# Specification: Runtime UI

## Objective

- Make runtime-owned facts visible to the UI.

## Source Of Truth

- Runtime owner: `src/runtime.rs`; UI consumers: `web/src/App.tsx`; generated artifacts: none; retired surfaces: none.

## Evidence Status

- Verified fact: `src/runtime.rs` owns the fixture runtime in this test repository.

## Runtime Contract

- `src/runtime.rs` returns canonical facts and fails closed when data is missing.

## UI Contract

- `web/src/App.tsx` renders runtime output and does not duplicate catalogs.

## Generated Artifacts

- none

## Fixture Policy

- Production code cannot import fixture/demo/sample data.

## Retired / Superseded Surfaces

- none

## Acceptance Criteria

- The runtime payload is rendered in the UI without a local fallback catalog.

## Verification

- `cargo test -p app runtime_readback`

## Review And Closeout

- Autonomous review reruns `cargo test -p app runtime_readback` and greps for duplicated catalogs.

## Autonomous Defaults

- Default to runtime-first implementation and explicit UI readback proof; no external blocker applies.
"#
    }

    #[test]
    fn auto_spec_output_accepts_production_shape() {
        let root = temp_root("valid-spec");
        let spec_path = root.join("specs/300426-runtime-ui.md");
        fs::write(&spec_path, valid_spec()).expect("write spec");

        verify_spec_output(&spec_path).expect("spec validates");
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn auto_spec_output_rejects_missing_acceptance_bullets() {
        let root = temp_root("missing-acceptance");
        let spec_path = root.join("specs/300426-runtime-ui.md");
        fs::write(
            &spec_path,
            valid_spec().replace(
                "## Acceptance Criteria\n\n- The runtime payload is rendered in the UI without a local fallback catalog.",
                "## Acceptance Criteria\n\nThe runtime payload is rendered.",
            ),
        )
        .expect("write spec");

        let error = verify_spec_output(&spec_path).expect_err("acceptance bullet rejected");
        assert!(error.to_string().contains("Acceptance Criteria"));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn auto_spec_output_rejects_open_questions_section() {
        let root = temp_root("open-questions");
        let spec_path = root.join("specs/300426-runtime-ui.md");
        fs::write(
            &spec_path,
            valid_spec().replace("## Autonomous Defaults", "## Open Questions"),
        )
        .expect("write spec");

        let error = verify_spec_output(&spec_path).expect_err("open questions rejected");
        assert!(error.to_string().contains("Autonomous Defaults"));
        let _ = fs::remove_dir_all(root);
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
        assert!(error
            .to_string()
            .contains("Dependencies:` must be machine-readable IDs only"));
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
        assert!(error.to_string().contains("multi-filter cargo test"));
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
        assert!(error.to_string().contains("malformed grep verification"));
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
