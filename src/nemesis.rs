use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use anyhow::{bail, Context, Result};
use chrono::Local;

use crate::util::{atomic_write, copy_tree, ensure_repo_layout, git_repo_root, timestamp_slug};
use crate::NemesisArgs;

const DEFAULT_NEMESIS_PROMPT: &str = r#"0a. Study `AGENTS.md` for repo-specific build, validation, and staging rules.
0b. Study `specs/*`, `IMPLEMENTATION_PLAN.md`, and any security- or audit-related docs already present.
0c. You are running a Nemesis-style audit inspired by the upstream `nemesis-auditor` workflow. Emulate the method directly in this run:
    - Phase 0: Recon and target selection
    - Pass 1: Feynman-style deep logic audit
    - Pass 2: State inconsistency audit enriched by Pass 1 findings
    - Pass 3+: Alternate targeted Feynman and State re-passes until convergence or a maximum of 6 total passes
    - Only keep evidence-backed findings

1. Your task is to perform a deep hardening audit of the live repository and write the audit outputs only into `nemesis/`.
   - Treat the codebase as truth.
   - Use docs and existing plans as supporting context, not authority.
   - Focus on business-logic flaws, state-desync risks, broken invariants, ordering problems, missing guards, and dangerous assumptions.

2. Do not modify root `specs/` or root `IMPLEMENTATION_PLAN.md` directly.
   - Write exactly these files:
     - `nemesis/nemesis-audit.md`
     - `nemesis/IMPLEMENTATION_PLAN.md`

3. `nemesis/nemesis-audit.md` requirements:
   - Must start with `# Specification: Nemesis Audit Findings and Hardening Requirements`
   - Capture only verified findings or verified hardening requirements
   - For each major finding or requirement, include:
     - affected surfaces
     - triggering scenario or failure mode
     - invariant or assumption that breaks
     - why this matters now
     - discovery path (`Feynman`, `State`, or `Cross-feed`)

4. `nemesis/IMPLEMENTATION_PLAN.md` requirements:
   - Must start with `# IMPLEMENTATION_PLAN`
   - Use these top-level sections exactly:
     - `## Priority Work`
     - `## Follow-On Work`
     - `## Completed / Already Satisfied`
   - Each actionable task must use this exact header format:
     - `- [ ] `TASK-ID` Short title`
   - Each task must include these exact fields:
     - `Spec:`
     - `Why now:`
     - `Codebase evidence:`
     - `Owns:`
     - `Integration touchpoints:`
     - `Scope boundary:`
     - `Required tests:`
     - `Dependencies:`
     - `Completion signal:`
   - Only put unfinished work in `Priority Work` or `Follow-On Work`
   - Put already-satisfied audit items only in `Completed / Already Satisfied`
   - Use task ids prefixed with `NEM-`

5. The resulting plan must be execution-ready:
   - concrete
   - file-grounded
   - bounded
   - high signal
   - no vague “investigate further” tasks unless the uncertainty itself is the verified issue

99999. Important: this is not a generic security scan. Use the Nemesis back-and-forth method.
999999. Important: do not invent findings that you cannot support with repo evidence.
9999999. Important: write the two required files completely into `nemesis/` and stop."#;

const EMPTY_PLAN: &str = "# IMPLEMENTATION_PLAN\n\n## Priority Work\n\n## Follow-On Work\n\n## Completed / Already Satisfied\n";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum PlanSection {
    Priority,
    FollowOn,
    Completed,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct PlanTaskBlock {
    section: PlanSection,
    task_id: String,
    checked: bool,
    markdown: String,
}

enum NemesisBackend<'a> {
    Codex {
        model: &'a str,
        reasoning_effort: &'a str,
        codex_bin: &'a Path,
    },
    Opencode {
        provider_label: &'static str,
        model: &'static str,
        variant: &'static str,
        opencode_bin: &'a Path,
    },
}

impl<'a> NemesisBackend<'a> {
    fn label(&self) -> &'static str {
        match self {
            Self::Codex { .. } => "codex",
            Self::Opencode { provider_label, .. } => provider_label,
        }
    }

    fn model(&self) -> &str {
        match self {
            Self::Codex { model, .. } => model,
            Self::Opencode { model, .. } => model,
        }
    }

    fn variant(&self) -> &str {
        match self {
            Self::Codex {
                reasoning_effort, ..
            } => reasoning_effort,
            Self::Opencode { variant, .. } => variant,
        }
    }
}

pub(crate) async fn run_nemesis(args: NemesisArgs) -> Result<()> {
    let repo_root = git_repo_root()?;
    ensure_repo_layout(&repo_root)?;

    let output_dir = args
        .output_dir
        .clone()
        .unwrap_or_else(|| repo_root.join("nemesis"));
    let backend = select_backend(&args);
    let previous_snapshot = if args.dry_run {
        None
    } else {
        prepare_output_dir(&repo_root, &output_dir)?
    };

    let prompt_template = match &args.prompt_file {
        Some(path) => fs::read_to_string(path)
            .with_context(|| format!("failed to read prompt file {}", path.display()))?,
        None => DEFAULT_NEMESIS_PROMPT.to_string(),
    };
    let full_prompt = format!("{prompt_template}\n\nExecute the instructions above.");
    let prompt_path = repo_root
        .join(".auto")
        .join("logs")
        .join(format!("nemesis-{}-prompt.md", timestamp_slug()));
    atomic_write(&prompt_path, full_prompt.as_bytes())
        .with_context(|| format!("failed to write {}", prompt_path.display()))?;

    println!("auto nemesis");
    println!("repo root:   {}", repo_root.display());
    println!("output dir:  {}", output_dir.display());
    println!("backend:     {}", backend.label());
    println!("model:       {}", backend.model());
    println!("variant:     {}", backend.variant());
    if let Some(previous) = &previous_snapshot {
        println!("prior input: {}", previous.display());
    }
    if args.dry_run {
        println!("mode:        dry-run");
        return Ok(());
    }

    let raw_response = run_nemesis_backend(&repo_root, &full_prompt, &backend)?;
    let response_path = repo_root
        .join(".auto")
        .join("logs")
        .join(format!("nemesis-{}-response.log", timestamp_slug()));
    if !raw_response.trim().is_empty() {
        atomic_write(&response_path, raw_response.as_bytes())
            .with_context(|| format!("failed to write {}", response_path.display()))?;
    }

    let spec_path = verify_nemesis_spec(&output_dir)?;
    let plan_path = verify_nemesis_plan(&output_dir)?;
    let root_spec = sync_nemesis_spec_to_root(&repo_root, &spec_path)?;
    let appended = append_nemesis_plan_to_root(&repo_root, &plan_path)?;

    println!();
    println!("nemesis complete");
    println!("spec:        {}", spec_path.display());
    println!("plan:        {}", plan_path.display());
    println!("root spec:   {}", root_spec.display());
    println!("root tasks:  {} appended", appended);
    println!("prompt log:  {}", prompt_path.display());
    if response_path.exists() {
        println!("model log:   {}", response_path.display());
    }

    Ok(())
}

fn select_backend(args: &NemesisArgs) -> NemesisBackend<'_> {
    if args.kimi {
        return NemesisBackend::Opencode {
            provider_label: "opencode-kimi",
            model: "kimi-for-coding/k2p5",
            variant: "high",
            opencode_bin: &args.opencode_bin,
        };
    }
    if args.minimax {
        return NemesisBackend::Opencode {
            provider_label: "opencode-minimax",
            model: "minimax/MiniMax-M2.5",
            variant: "high",
            opencode_bin: &args.opencode_bin,
        };
    }
    NemesisBackend::Codex {
        model: &args.model,
        reasoning_effort: &args.reasoning_effort,
        codex_bin: &args.codex_bin,
    }
}

fn prepare_output_dir(repo_root: &Path, output_dir: &Path) -> Result<Option<PathBuf>> {
    if !output_dir.exists() {
        fs::create_dir_all(output_dir)
            .with_context(|| format!("failed to create {}", output_dir.display()))?;
        return Ok(None);
    }
    if !output_dir.is_dir() {
        bail!(
            "Nemesis output path {} is not a directory",
            output_dir.display()
        );
    }

    let has_contents = fs::read_dir(output_dir)
        .with_context(|| format!("failed to read {}", output_dir.display()))?
        .next()
        .transpose()?
        .is_some();
    let archived = if has_contents {
        let snapshot_root = repo_root.join(".auto").join("fresh-input").join(format!(
            "{}-previous-{}",
            output_dir
                .file_name()
                .and_then(|value| value.to_str())
                .unwrap_or("nemesis"),
            timestamp_slug()
        ));
        copy_tree(output_dir, &snapshot_root).with_context(|| {
            format!(
                "failed to archive existing Nemesis output from {} into {}",
                output_dir.display(),
                snapshot_root.display()
            )
        })?;
        Some(snapshot_root)
    } else {
        None
    };

    fs::remove_dir_all(output_dir)
        .with_context(|| format!("failed to clear {}", output_dir.display()))?;
    fs::create_dir_all(output_dir)
        .with_context(|| format!("failed to recreate {}", output_dir.display()))?;
    Ok(archived)
}

fn run_nemesis_backend(
    repo_root: &Path,
    prompt: &str,
    backend: &NemesisBackend<'_>,
) -> Result<String> {
    match backend {
        NemesisBackend::Codex {
            model,
            reasoning_effort,
            codex_bin,
        } => run_codex(repo_root, prompt, model, reasoning_effort, codex_bin),
        NemesisBackend::Opencode {
            model,
            variant,
            opencode_bin,
            ..
        } => run_opencode(repo_root, prompt, model, variant, opencode_bin),
    }
}

fn run_codex(
    repo_root: &Path,
    prompt: &str,
    model: &str,
    reasoning_effort: &str,
    codex_bin: &Path,
) -> Result<String> {
    let mut child = Command::new(codex_bin)
        .arg("exec")
        .arg("--json")
        .arg("--dangerously-bypass-approvals-and-sandbox")
        .arg("--skip-git-repo-check")
        .arg("--cd")
        .arg(repo_root)
        .arg("-m")
        .arg(model)
        .arg("-c")
        .arg(format!("model_reasoning_effort=\"{reasoning_effort}\""))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .current_dir(repo_root)
        .spawn()
        .with_context(|| {
            format!(
                "failed to launch Codex at {} from {}",
                codex_bin.display(),
                repo_root.display()
            )
        })?;

    child
        .stdin
        .as_mut()
        .context("Codex stdin missing for Nemesis run")?
        .write_all(prompt.as_bytes())
        .context("failed to write Nemesis prompt to Codex")?;

    let output = child
        .wait_with_output()
        .context("failed waiting for Codex Nemesis run")?;
    let stdout = String::from_utf8(output.stdout).context("Codex stdout was not valid UTF-8")?;
    let stderr = String::from_utf8(output.stderr).context("Codex stderr was not valid UTF-8")?;
    if output.status.success() {
        return Ok(stdout);
    }
    bail!(
        "Codex Nemesis run failed: {}",
        stderr.trim().if_empty_then(stdout.trim())
    );
}

fn run_opencode(
    repo_root: &Path,
    prompt: &str,
    model: &str,
    variant: &str,
    opencode_bin: &Path,
) -> Result<String> {
    let output = Command::new(opencode_bin)
        .arg("run")
        .arg("--format")
        .arg("json")
        .arg("--model")
        .arg(model)
        .arg("--variant")
        .arg(variant)
        .arg(prompt)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .current_dir(repo_root)
        .output()
        .with_context(|| {
            format!(
                "failed to launch OpenCode at {} from {}",
                opencode_bin.display(),
                repo_root.display()
            )
        })?;

    let stdout = String::from_utf8(output.stdout).context("OpenCode stdout was not valid UTF-8")?;
    let stderr = String::from_utf8(output.stderr).context("OpenCode stderr was not valid UTF-8")?;
    if output.status.success() {
        return Ok(stdout);
    }
    bail!(
        "OpenCode Nemesis run failed: {}",
        stderr.trim().if_empty_then(stdout.trim())
    );
}

fn verify_nemesis_spec(output_dir: &Path) -> Result<PathBuf> {
    let spec_path = output_dir.join("nemesis-audit.md");
    if !spec_path.exists() {
        bail!("Nemesis run did not write {}", spec_path.display());
    }
    let markdown = fs::read_to_string(&spec_path)
        .with_context(|| format!("failed to read {}", spec_path.display()))?;
    if !markdown.starts_with("# Specification:") {
        bail!(
            "Nemesis spec {} must start with `# Specification:`",
            spec_path.display()
        );
    }
    Ok(spec_path)
}

fn verify_nemesis_plan(output_dir: &Path) -> Result<PathBuf> {
    let plan_path = output_dir.join("IMPLEMENTATION_PLAN.md");
    if !plan_path.exists() {
        bail!("Nemesis run did not write {}", plan_path.display());
    }
    let markdown = fs::read_to_string(&plan_path)
        .with_context(|| format!("failed to read {}", plan_path.display()))?;
    for required in [
        "# IMPLEMENTATION_PLAN",
        "## Priority Work",
        "## Follow-On Work",
        "## Completed / Already Satisfied",
    ] {
        if !markdown.contains(required) {
            bail!("Nemesis implementation plan is missing `{required}`");
        }
    }
    Ok(plan_path)
}

fn sync_nemesis_spec_to_root(repo_root: &Path, spec_path: &Path) -> Result<PathBuf> {
    let root_specs_dir = repo_root.join("specs");
    fs::create_dir_all(&root_specs_dir)
        .with_context(|| format!("failed to create {}", root_specs_dir.display()))?;

    let date_prefix = Local::now().format("%d%m%y").to_string();
    let slug = spec_path
        .file_stem()
        .and_then(|value| value.to_str())
        .unwrap_or("nemesis-audit");
    let extension = spec_path
        .extension()
        .and_then(|value| value.to_str())
        .unwrap_or("md");

    let mut counter = 1usize;
    let destination = loop {
        let candidate = if counter == 1 {
            root_specs_dir.join(format!("{date_prefix}-{slug}.{extension}"))
        } else {
            root_specs_dir.join(format!("{date_prefix}-{slug}-{counter}.{extension}"))
        };
        if !candidate.exists() {
            break candidate;
        }
        counter += 1;
    };

    fs::copy(spec_path, &destination).with_context(|| {
        format!(
            "failed to copy {} -> {}",
            spec_path.display(),
            destination.display()
        )
    })?;
    Ok(destination)
}

fn append_nemesis_plan_to_root(repo_root: &Path, nemesis_plan_path: &Path) -> Result<usize> {
    let root_plan_path = repo_root.join("IMPLEMENTATION_PLAN.md");
    let existing = if root_plan_path.exists() {
        fs::read_to_string(&root_plan_path)
            .with_context(|| format!("failed to read {}", root_plan_path.display()))?
    } else {
        EMPTY_PLAN.to_string()
    };
    let nemesis_plan = fs::read_to_string(nemesis_plan_path)
        .with_context(|| format!("failed to read {}", nemesis_plan_path.display()))?;

    let (merged, appended) = append_new_open_tasks(&existing, &nemesis_plan)?;
    atomic_write(&root_plan_path, merged.as_bytes())
        .with_context(|| format!("failed to write {}", root_plan_path.display()))?;
    Ok(appended)
}

fn append_new_open_tasks(existing: &str, nemesis_plan: &str) -> Result<(String, usize)> {
    let existing_blocks = extract_plan_task_blocks(existing)?;
    let existing_ids = existing_blocks
        .iter()
        .map(|block| block.task_id.as_str())
        .collect::<std::collections::BTreeSet<_>>();

    let new_blocks = extract_plan_task_blocks(nemesis_plan)?
        .into_iter()
        .filter(|block| !block.checked)
        .filter(|block| !existing_ids.contains(block.task_id.as_str()))
        .collect::<Vec<_>>();

    if new_blocks.is_empty() {
        return Ok((existing.to_string(), 0));
    }

    let mut merged = existing.to_string();
    append_blocks_to_section(&mut merged, PlanSection::Priority, &new_blocks)?;
    append_blocks_to_section(&mut merged, PlanSection::FollowOn, &new_blocks)?;
    Ok((merged, new_blocks.len()))
}

fn append_blocks_to_section(
    markdown: &mut String,
    section: PlanSection,
    blocks: &[PlanTaskBlock],
) -> Result<()> {
    let section_header = match section {
        PlanSection::Priority => "## Priority Work",
        PlanSection::FollowOn => "## Follow-On Work",
        PlanSection::Completed => return Ok(()),
    };
    let section_blocks = blocks
        .iter()
        .filter(|block| block.section == section)
        .collect::<Vec<_>>();
    if section_blocks.is_empty() {
        return Ok(());
    }

    let insert_at = markdown
        .find(section_header)
        .with_context(|| format!("root plan is missing section `{section_header}`"))?;
    let section_end = markdown[insert_at + section_header.len()..]
        .find("\n## ")
        .map(|offset| insert_at + section_header.len() + offset)
        .unwrap_or(markdown.len());

    let mut addition = String::new();
    if !markdown[..section_end].ends_with('\n') {
        addition.push('\n');
    }
    if !markdown[..section_end].ends_with("\n\n") {
        addition.push('\n');
    }
    for block in section_blocks {
        addition.push_str(block.markdown.trim_end());
        addition.push_str("\n\n");
    }
    markdown.insert_str(section_end, &addition);
    Ok(())
}

fn extract_plan_task_blocks(markdown: &str) -> Result<Vec<PlanTaskBlock>> {
    let mut blocks = Vec::new();
    let mut current_section = None::<PlanSection>;
    let mut current_lines = Vec::<String>::new();

    for line in markdown.lines() {
        if let Some(section) = parse_section_header(line) {
            if let Some(block) = finalize_plan_block(current_section, &current_lines)? {
                blocks.push(block);
            }
            current_section = Some(section);
            current_lines.clear();
            continue;
        }

        if parse_plan_task_header(line).is_some() {
            if let Some(block) = finalize_plan_block(current_section, &current_lines)? {
                blocks.push(block);
            }
            current_lines = vec![line.to_string()];
            continue;
        }

        if !current_lines.is_empty() {
            current_lines.push(line.to_string());
        }
    }

    if let Some(block) = finalize_plan_block(current_section, &current_lines)? {
        blocks.push(block);
    }

    Ok(blocks)
}

fn finalize_plan_block(
    section: Option<PlanSection>,
    lines: &[String],
) -> Result<Option<PlanTaskBlock>> {
    if lines.is_empty() {
        return Ok(None);
    }
    let Some((checked, task_id, _title)) = parse_plan_task_header(&lines[0]) else {
        return Ok(None);
    };
    Ok(Some(PlanTaskBlock {
        section: section.unwrap_or(PlanSection::Priority),
        task_id,
        checked,
        markdown: lines.join("\n"),
    }))
}

fn parse_section_header(line: &str) -> Option<PlanSection> {
    match line.trim() {
        "## Priority Work" => Some(PlanSection::Priority),
        "## Follow-On Work" => Some(PlanSection::FollowOn),
        "## Completed / Already Satisfied" => Some(PlanSection::Completed),
        _ => None,
    }
}

fn parse_plan_task_header(line: &str) -> Option<(bool, String, String)> {
    let trimmed = line.trim_start();
    let checked = if trimmed.starts_with("- [ ] ") {
        false
    } else if trimmed.starts_with("- [x] ") || trimmed.starts_with("- [X] ") {
        true
    } else {
        return None;
    };
    let rest = trimmed[6..].trim_start();
    let rest = rest.strip_prefix('`')?;
    let tick = rest.find('`')?;
    let task_id = rest[..tick].trim().to_string();
    let title = rest[tick + 1..].trim().to_string();
    Some((checked, task_id, title))
}

trait EmptyFallback {
    fn if_empty_then<'a>(&'a self, fallback: &'a str) -> &'a str;
}

impl EmptyFallback for str {
    fn if_empty_then<'a>(&'a self, fallback: &'a str) -> &'a str {
        if self.trim().is_empty() {
            fallback
        } else {
            self
        }
    }
}

#[cfg(test)]
mod tests {
    use super::append_new_open_tasks;

    #[test]
    fn appends_only_new_unchecked_nemesis_tasks() {
        let existing = r#"# IMPLEMENTATION_PLAN

## Priority Work

- [ ] `VAL-001` Validate query
Spec: specs/020426-query.md

## Follow-On Work

## Completed / Already Satisfied
"#;

        let nemesis = r#"# IMPLEMENTATION_PLAN

## Priority Work

- [ ] `NEM-001` Harden cross-surface invariant
Spec: specs/020426-nemesis-audit.md

- [ ] `VAL-001` Validate query
Spec: specs/020426-query.md

## Follow-On Work

- [ ] `NEM-002` Add state-sync regression coverage
Spec: specs/020426-nemesis-audit.md

## Completed / Already Satisfied

- [x] `NEM-003` Already satisfied
Spec: specs/020426-nemesis-audit.md
"#;

        let (merged, appended) = append_new_open_tasks(existing, nemesis).unwrap();
        assert_eq!(appended, 2);
        assert!(merged.contains("`NEM-001`"));
        assert!(merged.contains("`NEM-002`"));
        assert_eq!(merged.matches("`VAL-001`").count(), 1);
        assert!(!merged.contains("`NEM-003`"));
    }
}
