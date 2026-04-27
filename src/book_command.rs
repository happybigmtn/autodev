use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};

use crate::codex_exec::{run_codex_exec_max_context, MAX_CODEX_MODEL_CONTEXT_WINDOW};
use crate::util::{
    atomic_write, binary_provenance_line, ensure_repo_layout, git_repo_root, list_markdown_files,
    timestamp_slug,
};
use crate::BookArgs;

struct BookRun {
    audit_root: PathBuf,
    run_id: String,
    run_root: PathBuf,
    book_root: PathBuf,
}

struct PreservedFile {
    path: PathBuf,
    bytes: Vec<u8>,
}

pub(crate) async fn run_book(args: BookArgs) -> Result<()> {
    let repo_root = git_repo_root()?;
    ensure_repo_layout(&repo_root)?;
    let run = resolve_book_run(&repo_root, &args)?;
    let protected = preserved_appendix_and_catalog_files(&run.book_root)?;
    let logs_root = repo_root
        .join(".auto")
        .join("book")
        .join(&run.run_id)
        .join(timestamp_slug());
    fs::create_dir_all(&logs_root)
        .with_context(|| format!("failed to create {}", logs_root.display()))?;

    let prompt = build_book_prompt(&repo_root, &run, &protected)?;
    let prompt_path = logs_root.join("prompt.md");
    let stdout_path = logs_root.join("stdout.log");
    let stderr_path = logs_root.join("stderr.log");
    atomic_write(&prompt_path, prompt.as_bytes())
        .with_context(|| format!("failed to write {}", prompt_path.display()))?;

    println!("auto book");
    println!("binary:      {}", binary_provenance_line());
    println!("repo root:   {}", repo_root.display());
    println!("audit run:   {}", run.run_id);
    println!("audit root:  {}", run.run_root.display());
    println!("book root:   {}", run.book_root.display());
    println!("model:       {}", args.model);
    println!("effort:      {}", args.reasoning_effort);
    println!("context:     {} tokens", MAX_CODEX_MODEL_CONTEXT_WINDOW);
    println!("prompt:      {}", prompt_path.display());

    if args.dry_run {
        println!("{prompt}");
        return Ok(());
    }

    let status = run_codex_exec_max_context(
        &repo_root,
        &prompt,
        &args.model,
        &args.reasoning_effort,
        &args.codex_bin,
        &stderr_path,
        Some(&stdout_path),
        "book",
    )
    .await?;
    restore_preserved_files(&protected)?;

    if !status.success() {
        bail!(
            "auto book failed with status {status}; see {}",
            stderr_path.display()
        );
    }
    require_nonempty_file(&run.book_root.join("README.md"))?;

    if !args.skip_quality_review {
        let review_prompt = build_book_quality_review_prompt(&repo_root, &run)?;
        let review_prompt_path = logs_root.join("quality-review-prompt.md");
        let review_stdout_path = logs_root.join("quality-review-stdout.log");
        let review_stderr_path = logs_root.join("quality-review-stderr.log");
        atomic_write(&review_prompt_path, review_prompt.as_bytes())
            .with_context(|| format!("failed to write {}", review_prompt_path.display()))?;
        println!("review:      {}", review_prompt_path.display());
        let review_status = run_codex_exec_max_context(
            &repo_root,
            &review_prompt,
            &args.model,
            &args.reasoning_effort,
            &args.codex_bin,
            &review_stderr_path,
            Some(&review_stdout_path),
            "book-quality-review",
        )
        .await?;
        restore_preserved_files(&protected)?;
        if !review_status.success() {
            bail!(
                "auto book quality review failed with status {review_status}; see {}",
                review_stderr_path.display()
            );
        }
        let review_path = book_quality_review_path(&run);
        require_nonempty_file(&review_path)?;
        if !quality_review_is_pass(&review_path)? {
            bail!(
                "auto book quality review did not pass; see {}",
                review_path.display()
            );
        }
        println!("quality:     {}", review_path.display());
    }

    println!("stdout:      {}", stdout_path.display());
    println!("stderr:      {}", stderr_path.display());
    println!("done:        {}", run.book_root.display());
    Ok(())
}

fn resolve_book_run(repo_root: &Path, args: &BookArgs) -> Result<BookRun> {
    let audit_root = resolve_path(
        repo_root,
        args.audit_root
            .as_deref()
            .unwrap_or_else(|| Path::new("audit/everything")),
    );
    let run_id = if let Some(run_id) = args.audit_run_id.as_deref() {
        run_id.to_string()
    } else {
        latest_audit_run_id(repo_root, &audit_root)?
    };
    let run_root = audit_root.join(&run_id);
    if !run_root.is_dir() {
        bail!(
            "audit run `{}` was not found at {}",
            run_id,
            run_root.display()
        );
    }
    let book_root = args
        .output_dir
        .as_deref()
        .map(|path| resolve_path(repo_root, path))
        .unwrap_or_else(|| run_root.join("CODEBASE-BOOK"));
    Ok(BookRun {
        audit_root,
        run_id,
        run_root,
        book_root,
    })
}

fn resolve_path(repo_root: &Path, path: &Path) -> PathBuf {
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        repo_root.join(path)
    }
}

fn latest_audit_run_id(repo_root: &Path, audit_root: &Path) -> Result<String> {
    let latest_path = repo_root
        .join(".auto")
        .join("audit-everything")
        .join("latest-run");
    if let Ok(raw) = fs::read_to_string(&latest_path) {
        let run_id = raw.trim();
        if !run_id.is_empty() && audit_root.join(run_id).is_dir() {
            return Ok(run_id.to_string());
        }
    }

    let mut run_ids = Vec::new();
    if audit_root.is_dir() {
        for entry in fs::read_dir(audit_root)
            .with_context(|| format!("failed to read {}", audit_root.display()))?
        {
            let entry = entry?;
            let path = entry.path();
            if path.is_dir() {
                if let Some(name) = path.file_name().and_then(|name| name.to_str()) {
                    run_ids.push(name.to_string());
                }
            }
        }
    }
    run_ids.sort();
    run_ids.pop().with_context(|| {
        format!(
            "no audit runs found under {}; run `auto audit --everything` first or pass --audit-run-id",
            audit_root.display()
        )
    })
}

fn preserved_appendix_and_catalog_files(book_root: &Path) -> Result<Vec<PreservedFile>> {
    let mut preserved = Vec::new();
    for path in list_markdown_files(book_root)? {
        let relative = path.strip_prefix(book_root).unwrap_or(&path);
        if is_appendix_or_catalog_path(relative) {
            let bytes =
                fs::read(&path).with_context(|| format!("failed to read {}", path.display()))?;
            preserved.push(PreservedFile { path, bytes });
        }
    }
    Ok(preserved)
}

fn is_appendix_or_catalog_path(path: &Path) -> bool {
    let relative = path.to_string_lossy().replace('\\', "/").to_lowercase();
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("")
        .to_lowercase();
    file_name.starts_with("appendix")
        || file_name.contains("catalog")
        || relative.contains("file-catalog")
}

fn restore_preserved_files(files: &[PreservedFile]) -> Result<()> {
    for file in files {
        atomic_write(&file.path, &file.bytes)
            .with_context(|| format!("failed to restore {}", file.path.display()))?;
    }
    Ok(())
}

fn build_book_prompt(
    repo_root: &Path,
    run: &BookRun,
    protected: &[PreservedFile],
) -> Result<String> {
    let reports = markdown_under(&run.run_root.join("reports"), repo_root)?;
    let existing_book = markdown_under(&run.book_root, repo_root)?;
    let protected_list = protected
        .iter()
        .map(|file| display_relative(repo_root, &file.path))
        .collect::<Vec<_>>();
    let source_map = source_artifact_map(repo_root, run, &reports, &existing_book, &protected_list);

    Ok(format!(
        r#"You are writing the standalone `auto book` artifact for a mature codebase.

Repository root: `{repo}`
Audit run id: `{run_id}`
Audit run root: `{run_root}`
Output book root: `{book_root}`
Codex model context window: `{context_window}` tokens

This command is intentionally running Codex with the maximum model context window the CLI supports. Use that room. Read the audit source corpus deeply before writing; do not settle for the current high-level book shape.

Goal:
Rewrite `{book_root}` into a Feynman-style narrative walkthrough that lets a technical reader understand the codebase without reviewing the raw source files first.

Source priority:
1. Existing appendix/catalog files in `CODEBASE-BOOK/` for file-by-file facts.
2. Group reports under `{run_root}/reports/`.
3. `{run_root}/FINAL-REVIEW.md`, `{run_root}/REMEDIATION-PLAN.md`, and `{run_root}/RUN-STATUS.md` when present.
4. First-pass artifacts referenced by group reports when more detail is needed. Do not blindly glob stale first-pass artifact directories.
5. Source files themselves only to clarify important code paths, invariants, or examples that the audit artifacts do not explain enough.

Protected appendix/catalog files:
{protected_files}

Quality standard:
This book is for a smart junior developer who is otherwise unfamiliar with the repository. They should be able to read the book and come away with a very good, deep, technical understanding of the codebase, including the important crates/files, runtime flows, state boundaries, validation posture, and production risks, without needing to read the raw source first.

Rules:
- Rewrite narrative chapters only. Do not edit the protected appendix/catalog files listed above.
- Do not edit reports, source code, implementation plans, specs, manifests, or first-pass artifacts.
- Do not delete evidence or move audit artifacts.
- Keep the book chaptered; do not collapse it into one giant markdown file.
- `README.md` must have `# CODEBASE BOOK`, a table of contents, and a recommended reading path.
- Replace vague overview prose with detailed teaching prose. Explain the codebase from first principles, then walk through the major runtime/data/control flows.
- For key files and key sections, include narrative code walkthroughs: name the important modules, functions, types, tests, configuration, and command paths; explain why each matters and how control or data moves through it.
- Include short code excerpts only when they clarify an idea. Explain the excerpt in plain language immediately after it.
- Link from narrative chapters back to appendix/catalog entries instead of duplicating every file-by-file note.
- Cover architecture, domain model, runtime flow, persistence/state, external interfaces, tests/fixtures, operations/deployment, validation evidence, and residual risks when those surfaces exist.
- A reader should finish the book understanding what to change first, what not to break, and how to trace an important behavior through the code.
- Avoid hype, generic praise, inventory-only bullets, and empty labels like "utility file" without explaining the actual responsibility.

Suggested output shape:
- `README.md`
- `01-problem-and-mental-model.md`
- `02-architecture-map.md`
- `03-runtime-and-control-flow.md`
- `04-data-model-state-and-authority.md`
- subsystem chapters that match this repository
- `NN-validation-and-production-readiness.md`
- optional `BOOK-REFRESH.md` noting sources consulted and narrative files rewritten

Audit source map:
{source_map}
"#,
        repo = repo_root.display(),
        run_id = run.run_id,
        run_root = run.run_root.display(),
        book_root = run.book_root.display(),
        context_window = MAX_CODEX_MODEL_CONTEXT_WINDOW,
        protected_files = bullet_list_or_none(&protected_list),
        source_map = source_map,
    ))
}

fn build_book_quality_review_prompt(repo_root: &Path, run: &BookRun) -> Result<String> {
    let book_files = markdown_under(&run.book_root, repo_root)?;
    let reports = markdown_under(&run.run_root.join("reports"), repo_root)?;
    let source_map = source_artifact_map(repo_root, run, &reports, &book_files, &[]);
    let review_path = book_quality_review_path(run);

    Ok(format!(
        r#"You are the quality reviewer for a freshly generated `auto book` artifact.

Repository root: `{repo}`
Audit run id: `{run_id}`
Audit run root: `{run_root}`
Book root: `{book_root}`
Review output: `{review_path}`
Codex model context window: `{context_window}` tokens

Use the maximum-context run to inspect the book and enough audit source material to judge substance, not just formatting.

Quality standard:
The book is for a smart junior developer who is otherwise unfamiliar with this codebase. After reading it, they should have a very good, deep, technical understanding of the repository without opening the raw source files first. The book should teach first principles, key crates/files, runtime flows, data/state ownership, important functions/types, tests/fixtures, operations/deployment posture, validation evidence, and residual risk in clear Feynman-style prose.

Review rules:
- Do not edit source code, reports, specs, plans, or appendix/catalog files.
- Only write `{review_path}`.
- Judge whether the narrative chapters are detailed enough to substitute for a first source-code reading.
- Penalize high-level executive summaries, file inventories without explanations, missing key files/crates, missing control-flow walkthroughs, missing data/state explanations, and prose that would confuse a junior developer.
- Check that protected appendix/catalog material remains an appendix rather than the only place where technical detail exists.
- If the book fails, state exact missing chapters/sections and concrete rewrite instructions.

Write `{review_path}` with:
- `# BOOK QUALITY REVIEW`
- A line exactly `Verdict: PASS` or `Verdict: NO-GO`
- Reader standard assessment
- Technical depth assessment
- Feynman teaching assessment
- Key files/crates coverage assessment
- Runtime/data/control-flow coverage assessment
- Validation and production-readiness coverage assessment
- Required fixes before this book can be trusted as a codebase substitute
- Optional follow-ups

Audit and book source map:
{source_map}
"#,
        repo = repo_root.display(),
        run_id = run.run_id,
        run_root = run.run_root.display(),
        book_root = run.book_root.display(),
        review_path = review_path.display(),
        context_window = MAX_CODEX_MODEL_CONTEXT_WINDOW,
        source_map = source_map,
    ))
}

fn source_artifact_map(
    repo_root: &Path,
    run: &BookRun,
    reports: &[String],
    existing_book: &[String],
    protected: &[String],
) -> String {
    let mut body = String::new();
    body.push_str(&format!(
        "- Audit root: `{}`\n",
        display_relative(repo_root, &run.audit_root)
    ));
    body.push_str(&format!(
        "- Audit run root: `{}`\n",
        display_relative(repo_root, &run.run_root)
    ));
    for name in ["RUN-STATUS.md", "FINAL-REVIEW.md", "REMEDIATION-PLAN.md"] {
        let path = run.run_root.join(name);
        if path.exists() {
            body.push_str(&format!("- `{}`\n", display_relative(repo_root, &path)));
        }
    }
    body.push_str("\n## Group Reports\n");
    body.push_str(&bullet_list_or_none(reports));
    body.push_str("\n\n## Existing Book Markdown\n");
    body.push_str(&bullet_list_or_none(existing_book));
    body.push_str("\n\n## Protected Existing Book Markdown\n");
    body.push_str(&bullet_list_or_none(protected));
    body
}

fn book_quality_review_path(run: &BookRun) -> PathBuf {
    run.book_root.join("BOOK-QUALITY-REVIEW.md")
}

fn quality_review_is_pass(path: &Path) -> Result<bool> {
    let text =
        fs::read_to_string(path).with_context(|| format!("failed to read {}", path.display()))?;
    Ok(text
        .lines()
        .any(|line| line.trim().eq_ignore_ascii_case("Verdict: PASS")))
}

fn markdown_under(root: &Path, repo_root: &Path) -> Result<Vec<String>> {
    Ok(list_markdown_files(root)?
        .into_iter()
        .map(|path| display_relative(repo_root, &path))
        .collect())
}

fn display_relative(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .display()
        .to_string()
}

fn bullet_list_or_none(items: &[String]) -> String {
    if items.is_empty() {
        "- none found".to_string()
    } else {
        items
            .iter()
            .map(|item| format!("- `{item}`"))
            .collect::<Vec<_>>()
            .join("\n")
    }
}

fn require_nonempty_file(path: &Path) -> Result<()> {
    let metadata = fs::metadata(path).with_context(|| format!("missing {}", path.display()))?;
    if metadata.len() == 0 {
        bail!("{} is empty", path.display());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn appendix_and_catalog_paths_are_protected() {
        assert!(is_appendix_or_catalog_path(Path::new(
            "appendix-01-root-config.md"
        )));
        assert!(is_appendix_or_catalog_path(Path::new(
            "14-file-catalog-web.md"
        )));
        assert!(is_appendix_or_catalog_path(Path::new(
            "nested/runtime-catalog.md"
        )));
        assert!(!is_appendix_or_catalog_path(Path::new(
            "03-runtime-and-control-flow.md"
        )));
    }

    #[test]
    fn book_prompt_demands_feynman_walkthrough_and_max_context() {
        let dir = std::env::temp_dir().join(format!("auto-book-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let run_root = dir.join("audit/everything/run-1");
        let book_root = run_root.join("CODEBASE-BOOK");
        fs::create_dir_all(run_root.join("reports")).expect("failed to create reports");
        fs::create_dir_all(&book_root).expect("failed to create book root");
        fs::write(run_root.join("reports/src.md"), "# src\n").expect("failed to write report");
        fs::write(book_root.join("appendix-01-catalog.md"), "# catalog\n")
            .expect("failed to write appendix");
        let run = BookRun {
            audit_root: dir.join("audit/everything"),
            run_id: "run-1".to_string(),
            run_root,
            book_root: book_root.clone(),
        };
        let protected = preserved_appendix_and_catalog_files(&book_root)
            .expect("failed to snapshot protected files");
        let prompt = build_book_prompt(&dir, &run, &protected).expect("failed to build prompt");
        assert!(prompt.contains("Feynman-style narrative walkthrough"));
        assert!(prompt.contains("Codex model context window: `1000000` tokens"));
        assert!(prompt.contains("narrative code walkthroughs"));
        assert!(prompt.contains("smart junior developer"));
        assert!(prompt.contains("appendix-01-catalog.md"));
        fs::remove_dir_all(&dir).expect("failed to remove temp dir");
    }

    #[test]
    fn quality_review_prompt_uses_codebase_substitute_standard() {
        let dir =
            std::env::temp_dir().join(format!("auto-book-quality-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let run_root = dir.join("audit/everything/run-1");
        let book_root = run_root.join("CODEBASE-BOOK");
        fs::create_dir_all(run_root.join("reports")).expect("failed to create reports");
        fs::create_dir_all(&book_root).expect("failed to create book root");
        fs::write(book_root.join("README.md"), "# CODEBASE BOOK\n")
            .expect("failed to write book index");
        let run = BookRun {
            audit_root: dir.join("audit/everything"),
            run_id: "run-1".to_string(),
            run_root,
            book_root,
        };
        let prompt =
            build_book_quality_review_prompt(&dir, &run).expect("failed to build review prompt");
        assert!(prompt.contains("Verdict: PASS"));
        assert!(prompt.contains("Verdict: NO-GO"));
        assert!(prompt.contains("substitute for a first source-code reading"));
        assert!(prompt.contains("smart junior developer"));
        fs::remove_dir_all(&dir).expect("failed to remove temp dir");
    }
}
