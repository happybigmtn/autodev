//! Nemesis output verification: spec/plan presence checks, implementation
//! fix-result loading and validation, and backend-driven artifact repair.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use serde::Deserialize;

use crate::bug_command::llm_json::{
    escape_unescaped_quotes_in_json_strings, extract_complete_json_value_prefix,
    extract_fenced_json_block, JSON_REPAIR_MAX_BYTES,
};
use crate::nemesis::backend::{run_nemesis_backend, NemesisBackend};
use crate::nemesis::plan::load_unchecked_nemesis_task_ids;
use crate::nemesis::prompts::build_nemesis_results_repair_prompt;
use crate::util::{atomic_write, timestamp_slug};

#[derive(Debug, Deserialize)]
pub(crate) struct NemesisFixResult {
    task_id: String,
    status: String,
    summary: String,
    validation_commands: Vec<String>,
    touched_files: Vec<String>,
    residual_risks: Vec<String>,
}

#[derive(Debug)]
pub(crate) struct VerifiedNemesisOutputs {
    pub(crate) spec_path: PathBuf,
    pub(crate) plan_path: PathBuf,
}

pub(crate) fn verify_nemesis_outputs(output_dir: &Path) -> Result<VerifiedNemesisOutputs> {
    let spec_path = output_dir.join("nemesis-audit.md");
    let plan_path = output_dir.join("IMPLEMENTATION_PLAN.md");
    let has_spec = spec_path.exists();
    let has_plan = plan_path.exists();
    match (has_spec, has_plan) {
        (true, true) => {}
        (false, false) => {
            bail!(
                "Nemesis run did not write either {} or {}. Check the model logs and rerun.",
                spec_path.display(),
                plan_path.display()
            );
        }
        (false, true) => {
            bail!(
                "Nemesis run only partially completed: missing {} but found {}. Review the \
                 model logs, remove the partial output, and rerun.",
                spec_path.display(),
                plan_path.display()
            );
        }
        (true, false) => {
            bail!(
                "Nemesis run only partially completed: found {} but missing {}. Review the \
                 model logs, remove the partial output, and rerun.",
                spec_path.display(),
                plan_path.display()
            );
        }
    }

    let spec_markdown = fs::read_to_string(&spec_path)
        .with_context(|| format!("failed to read {}", spec_path.display()))?;
    if !spec_markdown.starts_with("# Specification:") {
        bail!(
            "Nemesis spec {} must start with `# Specification:`",
            spec_path.display()
        );
    }

    let plan_markdown = fs::read_to_string(&plan_path)
        .with_context(|| format!("failed to read {}", plan_path.display()))?;
    for required in [
        "# IMPLEMENTATION_PLAN",
        "## Priority Work",
        "## Follow-On Work",
        "## Completed / Already Satisfied",
    ] {
        if !plan_markdown.contains(required) {
            bail!("Nemesis implementation plan is missing `{required}`");
        }
    }
    Ok(VerifiedNemesisOutputs {
        spec_path,
        plan_path,
    })
}

pub(crate) fn draft_nemesis_outputs_valid(
    draft_audit_path: &Path,
    draft_plan_path: &Path,
) -> Result<()> {
    if !draft_audit_path.exists() || !draft_plan_path.exists() {
        bail!("draft Nemesis outputs are incomplete");
    }
    let audit = fs::read_to_string(draft_audit_path)
        .with_context(|| format!("failed to read {}", draft_audit_path.display()))?;
    let plan = fs::read_to_string(draft_plan_path)
        .with_context(|| format!("failed to read {}", draft_plan_path.display()))?;
    if !audit.starts_with("# Specification:") {
        bail!("draft Nemesis audit must start with `# Specification:`");
    }
    if !plan.contains("# IMPLEMENTATION_PLAN") {
        bail!("draft Nemesis plan must contain `# IMPLEMENTATION_PLAN`");
    }
    Ok(())
}

pub(crate) fn nonempty_file(path: &Path) -> bool {
    fs::metadata(path).is_ok_and(|metadata| metadata.is_file() && metadata.len() > 0)
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn verify_nemesis_implementation_results(
    repo_root: &Path,
    backend: &NemesisBackend,
    codex_bin: &Path,
    audit_path: &Path,
    results_json_path: &Path,
    results_md_path: &Path,
    plan_path: &Path,
) -> Result<PathBuf> {
    match verify_nemesis_implementation_results_once(results_json_path, results_md_path, plan_path)
    {
        Ok(_) => {}
        Err(original_error) => {
            println!(
                "warning: attempting backend repair for Nemesis implementation artifacts in {}",
                results_json_path.display()
            );
            repair_nemesis_implementation_outputs(
                repo_root,
                backend,
                codex_bin,
                audit_path,
                plan_path,
                results_json_path,
                results_md_path,
            )
            .await
            .with_context(|| {
                format!(
                    "backend repair failed for Nemesis implementation artifacts in {}",
                    results_json_path.display()
                )
            })?;
            verify_nemesis_implementation_results_once(results_json_path, results_md_path, plan_path)
                .map_err(|repair_error| {
                    anyhow::anyhow!(
                        "failed to recover Nemesis implementation artifacts after backend repair; original error: {}; repair error: {}",
                        original_error,
                        repair_error
                    )
                })?;
        }
    }
    Ok(results_json_path.to_path_buf())
}

pub(crate) fn verify_nemesis_implementation_results_once(
    results_json_path: &Path,
    results_md_path: &Path,
    plan_path: &Path,
) -> Result<Vec<NemesisFixResult>> {
    if !results_json_path.exists() {
        bail!(
            "Nemesis implementation did not write {}",
            results_json_path.display()
        );
    }
    if !results_md_path.exists() {
        bail!(
            "Nemesis implementation did not write {}",
            results_md_path.display()
        );
    }

    let results = load_nemesis_fix_results(results_json_path)?;
    let expected_ids = load_unchecked_nemesis_task_ids(plan_path)?;
    let actual_ids = results
        .iter()
        .map(|result| result.task_id.as_str())
        .collect::<BTreeSet<_>>();
    for task_id in &expected_ids {
        if !actual_ids.contains(task_id.as_str()) {
            bail!("Nemesis implementation results missing task `{task_id}`");
        }
    }
    Ok(results)
}

fn load_nemesis_fix_results(path: &Path) -> Result<Vec<NemesisFixResult>> {
    let content =
        fs::read_to_string(path).with_context(|| format!("failed to read {}", path.display()))?;
    let results = match serde_json::from_str::<Vec<NemesisFixResult>>(&content) {
        Ok(results) => results,
        Err(original_error) => {
            let Some(repaired) = repair_nemesis_json(&content) else {
                bail!("failed to parse {}: {}", path.display(), original_error);
            };
            match serde_json::from_str::<Vec<NemesisFixResult>>(&repaired) {
                Ok(results) => {
                    println!(
                        "warning: repaired invalid or incomplete JSON in {}",
                        path.display()
                    );
                    if repaired != content {
                        atomic_write(path, repaired.as_bytes())?;
                    }
                    results
                }
                Err(repair_error) => bail!(
                    "failed to parse {}: {}; automatic repair also failed: {}",
                    path.display(),
                    original_error,
                    repair_error
                ),
            }
        }
    };
    validate_nemesis_fix_results(&results)?;
    Ok(results)
}

fn validate_nemesis_fix_results(results: &[NemesisFixResult]) -> Result<()> {
    for result in results {
        match result.status.trim().to_ascii_lowercase().as_str() {
            "fixed" | "deferred" | "blocked" => {}
            other => bail!(
                "invalid Nemesis fix status `{other}` for {}",
                result.task_id
            ),
        }
        if result.task_id.trim().is_empty() || result.summary.trim().is_empty() {
            bail!(
                "Nemesis implementation result is missing required fields for `{}`",
                result.task_id
            );
        }
        if result.status.eq_ignore_ascii_case("fixed") {
            if result.validation_commands.is_empty() {
                bail!(
                    "Nemesis implementation result for `{}` must include validation commands",
                    result.task_id
                );
            }
            if result.touched_files.is_empty() && !fixed_nemesis_result_is_truthful_noop(result) {
                bail!(
                    "Nemesis implementation result for `{}` must include touched files unless the summary explicitly states that no file changes were needed",
                    result.task_id
                );
            }
        }
        if (result.status.eq_ignore_ascii_case("deferred")
            || result.status.eq_ignore_ascii_case("blocked"))
            && result.residual_risks.is_empty()
        {
            bail!(
                "Nemesis {} result for `{}` must explain residual risks",
                result.status,
                result.task_id
            );
        }
    }
    Ok(())
}

fn fixed_nemesis_result_is_truthful_noop(result: &NemesisFixResult) -> bool {
    if !result.status.eq_ignore_ascii_case("fixed") || !result.touched_files.is_empty() {
        return false;
    }

    let summary = result.summary.to_ascii_lowercase();
    summary.contains("no file changes were needed")
        || summary.contains("no code changes were needed")
        || summary.contains("no changes were needed")
}

fn repair_nemesis_json(content: &str) -> Option<String> {
    let candidate = extract_fenced_json_block(content).unwrap_or_else(|| content.to_string());
    if candidate.len() > JSON_REPAIR_MAX_BYTES {
        return None;
    }
    let repaired = escape_unescaped_quotes_in_json_strings(&candidate);
    let repaired = extract_complete_json_value_prefix(&repaired).unwrap_or(repaired);
    (repaired != content).then_some(repaired)
}

async fn repair_nemesis_implementation_outputs(
    repo_root: &Path,
    backend: &NemesisBackend,
    codex_bin: &Path,
    audit_path: &Path,
    plan_path: &Path,
    results_json_path: &Path,
    results_md_path: &Path,
) -> Result<()> {
    let prompt = build_nemesis_results_repair_prompt(
        audit_path,
        plan_path,
        results_json_path,
        results_md_path,
    );
    let repair_response = run_nemesis_backend(repo_root, &prompt, backend, codex_bin).await?;
    if !repair_response.trim().is_empty() {
        let log_path = repo_root.join(".auto").join("logs").join(format!(
            "nemesis-{}-implementation-repair-response.log",
            timestamp_slug()
        ));
        atomic_write(&log_path, repair_response.as_bytes())
            .with_context(|| format!("failed to write {}", log_path.display()))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};

    use super::{load_nemesis_fix_results, verify_nemesis_outputs};

    fn temp_repo_path(name: &str) -> PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system time should be after unix epoch")
            .as_nanos();
        std::env::temp_dir().join(format!(
            "autodev-nemesis-{name}-{}-{nonce}",
            std::process::id()
        ))
    }

    #[test]
    fn load_nemesis_fix_results_repairs_invalid_backslash_escapes() {
        let path = temp_repo_path("nemesis-invalid-escapes").join("implementation-results.json");
        fs::create_dir_all(path.parent().expect("temp file should have a parent"))
            .expect("failed to create temp dir");
        fs::write(
            &path,
            r#"[
  {
    "task_id": "NEM-001",
    "status": "blocked",
    "summary": "The pattern \d+\_suffix still appears in the copied output.",
    "validation_commands": [],
    "touched_files": [],
    "residual_risks": ["Needs manual review"]
  }
]"#,
        )
        .expect("failed to write invalid json");

        let results = load_nemesis_fix_results(&path).expect("repair should recover JSON");
        assert_eq!(results.len(), 1);
        assert!(results[0].summary.contains("\\d+\\_suffix"));
    }

    #[test]
    fn load_nemesis_fix_results_repairs_trailing_backend_wrapper() {
        let path = temp_repo_path("nemesis-trailing-wrapper").join("implementation-results.json");
        fs::create_dir_all(path.parent().expect("temp file should have a parent"))
            .expect("failed to create temp dir");
        fs::write(
            &path,
            r#"[
  {
    "task_id": "NEM-001",
    "status": "blocked",
    "summary": "The implementation stopped before editing code.",
    "validation_commands": [],
    "touched_files": [],
    "residual_risks": ["Needs a follow-up run"]
  }
]
</invoke>"#,
        )
        .expect("failed to write invalid json");

        let results = load_nemesis_fix_results(&path).expect("repair should recover JSON");
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].task_id, "NEM-001");
    }

    #[test]
    fn load_nemesis_fix_results_allows_truthful_noop_fixed_results() {
        let path = temp_repo_path("nemesis-fixed-noop").join("implementation-results.json");
        fs::create_dir_all(path.parent().expect("temp file should have a parent"))
            .expect("failed to create temp dir");
        fs::write(
            &path,
            r#"[
  {
    "task_id": "NEM-010",
    "status": "fixed",
    "summary": "No file changes were needed because the live repo already satisfied the requirement.",
    "validation_commands": ["rg -n 'alerts' docs/ops/alerts.md -S"],
    "touched_files": [],
    "residual_risks": []
  }
]"#,
        )
        .expect("failed to write noop json");

        let results = load_nemesis_fix_results(&path).expect("noop fixed result should load");
        assert_eq!(results.len(), 1);
        assert!(results[0].touched_files.is_empty());
    }

    #[test]
    fn load_nemesis_fix_results_rejects_fixed_results_without_files_or_noop_summary() {
        let path =
            temp_repo_path("nemesis-fixed-missing-files").join("implementation-results.json");
        fs::create_dir_all(path.parent().expect("temp file should have a parent"))
            .expect("failed to create temp dir");
        fs::write(
            &path,
            r#"[
  {
    "task_id": "NEM-011",
    "status": "fixed",
    "summary": "Updated the validation surface.",
    "validation_commands": ["cargo test -p barely-human observatory"],
    "touched_files": [],
    "residual_risks": []
  }
]"#,
        )
        .expect("failed to write invalid noop json");

        let error = load_nemesis_fix_results(&path).expect_err("result should be rejected");
        assert!(error
            .to_string()
            .contains("must include touched files unless the summary explicitly states"));
    }

    #[test]
    fn verify_nemesis_outputs_reports_partial_state() {
        let repo = temp_repo_path("partial-nemesis-output");
        let output_dir = repo.join("nemesis");
        fs::create_dir_all(&output_dir).expect("failed to create output dir");
        fs::write(
            output_dir.join("nemesis-audit.md"),
            "# Specification: partial\n",
        )
        .expect("failed to write partial spec");

        let error = verify_nemesis_outputs(&output_dir)
            .expect_err("partial output should fail verification")
            .to_string();
        assert!(error.contains("only partially completed"));
        assert!(error.contains("IMPLEMENTATION_PLAN.md"));
        assert!(error.contains("rerun"));

        fs::remove_dir_all(&repo).expect("failed to remove temp repo");
    }
}
