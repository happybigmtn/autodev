use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};

use crate::ReviewArgs;
use crate::claude_exec::run_claude_exec;
use crate::codex_exec::run_codex_exec;
use crate::util::{
    atomic_write, auto_checkpoint_if_needed, ensure_repo_layout, git_repo_root, git_stdout,
    push_branch_with_remote_sync, sync_branch_with_remote, timestamp_slug,
};

pub(crate) const DEFAULT_REVIEW_PROMPT: &str = r#"0a. Study `AGENTS.md` for repo-specific build, validation, and staging rules.
0b. Study `specs/*`, `IMPLEMENTATION_PLAN.md`, `COMPLETED.md`, `REVIEW.md`, `ARCHIVED.md`, `WORKLIST.md`, and `LEARNINGS.md` if they exist.
0c. You may use installed helper workflows like `/ce:review`, `/review`, `/ce:work`, or `/ce:compound` if they are available, but you must still satisfy the full review contract below even if those helpers are missing.
0d. When additional repositories are listed below, inspect and edit them directly when a reviewed item's owned surfaces, changed files, acceptance criteria, or blocker evidence point there. Read each touched repo's own `AGENTS.md` and operational docs before editing it.

1. Your task is to review the items currently listed in `REVIEW.md`.
   - Treat each review item as a claim that must be verified against the codebase, the specs, and the implementation plan.
   - Re-read the owned surfaces, integration touchpoints, and validation evidence for those items before trusting the claim.
   - If the reviewed item's implementation or fix lives in an additional listed repo, review and patch that repo directly while keeping this queue repo's review artifacts truthful.
   - Run a broad engineering review, not a status recap: look for regressions, weak assumptions, missing edge cases, security issues, integration gaps, and test blind spots.

2. Use this review workflow for every item:
   - Understand the intended behavior and expected change first.
   - Review the tests and verification evidence before reviewing the implementation details.
   - Reconstruct the changed-file set and blast radius for the reviewed item from commits, diffs, touched tests, and adjacent integration surfaces before you decide the item is safe.
   - Review the implementation across these five axes:
     - correctness
     - readability and simplicity
     - architecture and boundaries
     - security and trust boundaries
     - performance and scalability
   - If a base branch is discoverable, compare the current branch diff against that base instead of reviewing files in isolation.
   - Pay special attention to structural issues that tests often miss: SQL/query safety, trust-boundary violations, unintended conditional side effects, stale config or migration coupling, and changes whose blast radius is wider than the touched files imply.
   - For browser-facing or runtime-sensitive items, use browser/runtime verification when available instead of static review alone.
   - Verify the verification story itself: commands actually run, outputs believable, screenshots or runtime evidence consistent with the code.
   - Run a bounded simplification pass on the touched code when it will clearly improve readability or reduce complexity without changing behavior. Keep that simplification inside the reviewed surfaces; no drive-by cleanup.
   - If the reviewed item quietly bundles multiple logical changes, call that out and split the follow-up work truthfully instead of waving it through as one thing.
   - Categorize any findings as `Critical`, required, `Optional`, or `FYI`.

3. Respect the queue split:
   - `REVIEW.md` is the in-flight review queue.
   - `COMPLETED.md` is free to keep receiving new implementation completions while review is running.
   - Do not move items back into `IMPLEMENTATION_PLAN.md`.

4. If you find problems:
   - Append concrete, severity-tagged follow-up items to `WORKLIST.md`. Create it if missing.
   - Fix review findings directly when the root cause is clear and the work is bounded.
   - Record durable learnings in `LEARNINGS.md`.
   - Leave any not-yet-cleared entries in `REVIEW.md` until the fixes are actually landed and supported by the codebase.
   - Keep `AGENTS.md` operational only.

5. If a review item passes review:
   - Move its entry from `REVIEW.md` into `ARCHIVED.md`.
   - `ARCHIVED.md` should be append-only history.
   - Only archive items that are genuinely complete after review and any follow-up fixes.

6. Commit and push only truthful review increments:
   - Stay on the branch that is already checked out when `auto review` starts.
   - Do not create or switch branches during the review pass.
   - Stage only the files relevant to the review fixes plus `COMPLETED.md`, `REVIEW.md`, `ARCHIVED.md`, `WORKLIST.md`, `LEARNINGS.md`, and `AGENTS.md` when they changed.
   - If you touch multiple repositories, commit and push each repository separately. Never try to mix files from different git repos into one commit.
   - Commit with a message like `repo-name: review completed items` using the actual repository name for each touched repo.
   - Push back to that same branch in the queue repo after each successful commit-producing pass. For additional listed repos, push the currently checked-out branch unless that repo's own instructions require something else.

7. If `REVIEW.md` is empty or has no reviewable items:
   - Do not invent work.
   - Say so briefly and stop without making changes.

99999. Important: prefer fixing findings over explaining them.
999999. Important: do not archive an item until the code and review evidence support it.
9999999. Important: this is a bug-finding and hardening pass, not a feature pass.
99999999. Important: if the tests do not prove the claim, the implementation does not get a free pass."#;

const EMPTY_COMPLETED_DOC: &str = "# COMPLETED\n\n";
const REVIEW_HEADER: &str = "# REVIEW";
const ARCHIVED_HEADER: &str = "# ARCHIVED";

pub(crate) async fn run_review(args: ReviewArgs) -> Result<()> {
    let repo_root = git_repo_root()?;
    ensure_repo_layout(&repo_root)?;
    let reference_repos = resolve_reference_repos(&repo_root, &args.reference_repos)?;

    let completed_path = repo_root.join("COMPLETED.md");
    let review_path = repo_root.join("REVIEW.md");
    let archived_path = repo_root.join("ARCHIVED.md");
    ensure_review_docs(&review_path, &archived_path)?;
    let moved_items = handoff_completed_items_to_review_queue(&completed_path, &review_path)?;
    if !review_path.exists() || !has_reviewable_items(&review_path)? {
        println!("auto review");
        println!("repo root:   {}", repo_root.display());
        println!("status:      no reviewable items in REVIEW.md");
        return Ok(());
    }

    let current_branch = git_stdout(&repo_root, ["branch", "--show-current"])?;
    let current_branch = current_branch.trim().to_string();
    let push_branch = args
        .branch
        .clone()
        .unwrap_or_else(|| current_branch.clone());
    if let Some(required_branch) = args.branch.as_deref() {
        if current_branch != required_branch {
            bail!(
                "auto review must run on branch `{}` (current: `{}`)",
                required_branch,
                current_branch
            );
        }
    }

    let prompt_template = match &args.prompt_file {
        Some(path) => {
            let prompt = fs::read_to_string(path)
                .with_context(|| format!("failed to read prompt file {}", path.display()))?;
            append_reference_repo_clause(prompt, &reference_repos)
        }
        None => append_reference_repo_clause(DEFAULT_REVIEW_PROMPT.to_string(), &reference_repos),
    };
    let full_prompt = format!("{prompt_template}\n\nExecute the instructions above.");

    let run_root = args
        .run_root
        .unwrap_or_else(|| repo_root.join(".auto").join("review"));
    fs::create_dir_all(&run_root)
        .with_context(|| format!("failed to create {}", run_root.display()))?;
    let stderr_log_path = run_root.join("codex.stderr.log");

    let harness = if args.claude { "Claude" } else { "Codex" };

    println!("auto review");
    println!("repo root:   {}", repo_root.display());
    println!("branch:      {}", push_branch);
    if args.claude {
        println!("harness:     Claude (Opus 4.6 high)");
        println!(
            "max turns:   {}",
            args.max_turns
                .map(|n| n.to_string())
                .unwrap_or_else(|| "unlimited".to_string())
        );
    } else {
        println!("model:       {}", args.model);
        println!("reasoning:   {}", args.reasoning_effort);
    }
    println!("review doc:  {}", review_path.display());
    if !reference_repos.is_empty() {
        println!("references:  {}", reference_repos.len());
        for path in &reference_repos {
            println!("  - {}", path.display());
        }
    }
    if moved_items > 0 {
        println!(
            "handoff:     moved {} item(s) from COMPLETED.md",
            moved_items
        );
    }
    println!("run root:    {}", run_root.display());

    if let Some(commit) =
        auto_checkpoint_if_needed(&repo_root, push_branch.as_str(), "review checkpoint")?
    {
        println!("checkpoint:  committed pre-existing review changes at {commit}");
    } else if sync_branch_with_remote(&repo_root, push_branch.as_str())? {
        println!("remote sync: rebased onto origin/{}", push_branch);
    }

    let mut iteration = 0usize;
    while args.max_iterations == 0 || iteration < args.max_iterations {
        let prompt_path = repo_root
            .join(".auto")
            .join("logs")
            .join(format!("review-{}-prompt.md", timestamp_slug()));
        atomic_write(&prompt_path, full_prompt.as_bytes())
            .with_context(|| format!("failed to write {}", prompt_path.display()))?;
        println!("prompt log:  {}", prompt_path.display());

        let state_before = collect_tracked_repo_states(&repo_root, &reference_repos)?;
        println!();
        println!("running {harness} review iteration {}", iteration + 1);

        let exit_status = if args.claude {
            run_claude_exec(
                &repo_root,
                &full_prompt,
                args.max_turns,
                &stderr_log_path,
                "auto review",
            )
            .await?
        } else {
            run_codex_exec(
                &repo_root,
                &full_prompt,
                &args.model,
                &args.reasoning_effort,
                &args.codex_bin,
                &stderr_log_path,
                "auto review",
            )
            .await?
        };
        if !exit_status.success() {
            bail!(
                "{harness} exited with status {}; see {}",
                exit_status
                    .code()
                    .map(|code| code.to_string())
                    .unwrap_or_else(|| "signal".to_string()),
                stderr_log_path.display()
            );
        }

        println!();
        println!("{harness} review iteration complete");

        let state_after = collect_tracked_repo_states(&repo_root, &reference_repos)?;
        match summarize_repo_progress(&state_before, &state_after) {
            RepoProgress::NewCommits => {}
            RepoProgress::DirtyChanges(repos) => {
                bail!(
                    "tracked repo changes were left uncommitted in: {}; commit or revert them before continuing",
                    repos.join(", ")
                );
            }
            RepoProgress::None => {
                if let Some(commit) = auto_checkpoint_if_needed(
                    &repo_root,
                    push_branch.as_str(),
                    "review checkpoint",
                )? {
                    iteration += 1;
                    println!("checkpoint:  committed iteration changes at {commit}");
                    println!();
                    println!("================ REVIEW {} ================", iteration);
                    continue;
                }
                println!("no new commit detected; stopping.");
                break;
            }
        }

        if push_branch_with_remote_sync(&repo_root, push_branch.as_str())? {
            println!("remote sync: rebased onto origin/{}", push_branch);
        }
        if let Some(commit) =
            auto_checkpoint_if_needed(&repo_root, push_branch.as_str(), "review checkpoint")?
        {
            println!("checkpoint:  committed trailing changes at {commit}");
        }
        iteration += 1;
        println!();
        println!("================ REVIEW {} ================", iteration);
    }

    Ok(())
}

pub(crate) fn has_reviewable_items(path: &Path) -> Result<bool> {
    let content =
        fs::read_to_string(path).with_context(|| format!("failed to read {}", path.display()))?;
    Ok(!extract_review_items(&content).is_empty())
}

fn append_reference_repo_clause(prompt: String, reference_repos: &[PathBuf]) -> String {
    if reference_repos.is_empty() {
        return prompt;
    }

    let listing = reference_repos
        .iter()
        .map(|path| format!("- `{}`", path.display()))
        .collect::<Vec<_>>()
        .join("\n");
    format!(
        "{prompt}\n\nAdditional repositories you may inspect or edit when the review contract points there:\n{listing}\n\nRepository-crossing rules:\n- If a reviewed item's owned or changed surfaces live in one of these repos, review and fix that repo directly instead of pretending the queue repo owns it.\n- Keep `REVIEW.md`, `ARCHIVED.md`, `WORKLIST.md`, and `LEARNINGS.md` truthful in the queue repo even when code lands in another repo.\n- Read each touched repo's `AGENTS.md`, tests, and operational docs before editing it.\n- Commit and push each touched repo separately.\n"
    )
}

fn resolve_reference_repos(repo_root: &Path, paths: &[PathBuf]) -> Result<Vec<PathBuf>> {
    let mut resolved = discover_sibling_git_repos(repo_root)?;
    for path in paths {
        let absolute = if path.is_absolute() {
            path.clone()
        } else {
            repo_root.join(path)
        };
        let canonical = absolute
            .canonicalize()
            .with_context(|| format!("failed to resolve reference repo {}", absolute.display()))?;
        if !canonical.is_dir() {
            bail!("reference repo {} is not a directory", canonical.display());
        }

        let git_root =
            git_stdout(&canonical, ["rev-parse", "--show-toplevel"]).with_context(|| {
                format!(
                    "reference repo {} is not a git repository",
                    canonical.display()
                )
            })?;
        let git_root = PathBuf::from(git_root.trim())
            .canonicalize()
            .with_context(|| {
                format!(
                    "failed to canonicalize git root for {}",
                    canonical.display()
                )
            })?;
        if git_root != repo_root {
            resolved.push(git_root);
        }
    }
    resolved.sort();
    resolved.dedup();
    Ok(resolved)
}

fn discover_sibling_git_repos(repo_root: &Path) -> Result<Vec<PathBuf>> {
    let Some(parent) = repo_root.parent() else {
        return Ok(Vec::new());
    };

    let mut siblings = Vec::new();
    for entry in fs::read_dir(parent).with_context(|| {
        format!(
            "failed to read sibling directories under {}",
            parent.display()
        )
    })? {
        let entry =
            entry.with_context(|| format!("failed to read entry under {}", parent.display()))?;
        let candidate = entry.path();
        if !candidate.is_dir() {
            continue;
        }

        let canonical = candidate.canonicalize().with_context(|| {
            format!(
                "failed to canonicalize sibling directory {}",
                candidate.display()
            )
        })?;
        if canonical == repo_root {
            continue;
        }

        let Ok(git_root) = git_stdout(&canonical, ["rev-parse", "--show-toplevel"]) else {
            continue;
        };
        let git_root = PathBuf::from(git_root.trim())
            .canonicalize()
            .with_context(|| {
                format!(
                    "failed to canonicalize git root for {}",
                    canonical.display()
                )
            })?;
        if git_root == canonical {
            siblings.push(git_root);
        }
    }

    siblings.sort();
    siblings.dedup();
    Ok(siblings)
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct TrackedRepoState {
    name: String,
    path: PathBuf,
    head: String,
    status: String,
}

impl TrackedRepoState {
    #[cfg(test)]
    fn new(name: &str, path: &str, head: &str, status: &str) -> Self {
        Self {
            name: name.to_string(),
            path: PathBuf::from(path),
            head: head.to_string(),
            status: status.to_string(),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum RepoProgress {
    None,
    NewCommits,
    DirtyChanges(Vec<String>),
}

fn collect_tracked_repo_states(
    repo_root: &Path,
    reference_repos: &[PathBuf],
) -> Result<Vec<TrackedRepoState>> {
    let mut repos = Vec::with_capacity(reference_repos.len() + 1);
    repos.push(repo_root.to_path_buf());
    repos.extend(reference_repos.iter().cloned());

    let mut states = Vec::with_capacity(repos.len());
    for path in repos {
        let Ok(head) = git_stdout(&path, ["rev-parse", "HEAD"]) else {
            continue;
        };
        let status = git_stdout(&path, ["status", "--short"]).unwrap_or_default();
        states.push(TrackedRepoState {
            name: path
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("repo")
                .to_string(),
            path,
            head: head.trim().to_string(),
            status: status.trim().to_string(),
        });
    }
    Ok(states)
}

fn summarize_repo_progress(
    before: &[TrackedRepoState],
    after: &[TrackedRepoState],
) -> RepoProgress {
    let mut dirty_repos = Vec::new();
    for after_state in after {
        let Some(before_state) = before.iter().find(|state| state.path == after_state.path) else {
            return RepoProgress::NewCommits;
        };
        if before_state.head != after_state.head {
            return RepoProgress::NewCommits;
        }
        if before_state.status != after_state.status {
            dirty_repos.push(after_state.name.clone());
        }
    }

    if dirty_repos.is_empty() {
        RepoProgress::None
    } else {
        dirty_repos.sort();
        dirty_repos.dedup();
        RepoProgress::DirtyChanges(dirty_repos)
    }
}

pub(crate) fn handoff_completed_items_to_review_queue(
    completed_path: &Path,
    review_path: &Path,
) -> Result<usize> {
    let completed_items = if completed_path.exists() {
        extract_review_items(
            &fs::read_to_string(completed_path)
                .with_context(|| format!("failed to read {}", completed_path.display()))?,
        )
    } else {
        Vec::new()
    };
    if completed_items.is_empty() {
        return Ok(0);
    }

    let mut review_items = if review_path.exists() {
        extract_review_items(
            &fs::read_to_string(review_path)
                .with_context(|| format!("failed to read {}", review_path.display()))?,
        )
    } else {
        Vec::new()
    };
    let moved_count = completed_items.len();
    review_items.extend(completed_items);

    write_queue(review_path, REVIEW_HEADER, &review_items)?;
    atomic_write(completed_path, EMPTY_COMPLETED_DOC.as_bytes())
        .with_context(|| format!("failed to reset {}", completed_path.display()))?;
    Ok(moved_count)
}

fn extract_review_items(content: &str) -> Vec<String> {
    if content.lines().any(|line| line.starts_with("## ")) {
        return extract_section_review_items(content);
    }
    extract_bullet_review_items(content)
}

fn extract_section_review_items(content: &str) -> Vec<String> {
    let mut items = Vec::new();
    let mut current = Vec::new();
    for line in content.lines() {
        if line.starts_with("## ") {
            if !current.is_empty() {
                items.push(current.join("\n").trim_end().to_string());
                current.clear();
            }
            current.push(line.to_string());
        } else if !current.is_empty() {
            current.push(line.to_string());
        }
    }
    if !current.is_empty() {
        items.push(current.join("\n").trim_end().to_string());
    }
    items
}

fn write_queue(path: &Path, title: &str, items: &[String]) -> Result<()> {
    let mut content = String::from(title);
    content.push_str("\n\n");
    if !items.is_empty() {
        content.push_str(&items.join("\n\n"));
        content.push('\n');
    }
    atomic_write(path, content.as_bytes())
}

fn ensure_review_docs(review_path: &Path, archived_path: &Path) -> Result<()> {
    if !review_path.exists() {
        atomic_write(review_path, format!("{REVIEW_HEADER}\n\n").as_bytes())
            .with_context(|| format!("failed to initialize {}", review_path.display()))?;
    }
    if !archived_path.exists() {
        atomic_write(archived_path, format!("{ARCHIVED_HEADER}\n\n").as_bytes())
            .with_context(|| format!("failed to initialize {}", archived_path.display()))?;
    }
    Ok(())
}

fn extract_bullet_review_items(content: &str) -> Vec<String> {
    let mut items = Vec::new();
    let mut current = Vec::new();

    for raw_line in content.lines() {
        let line = raw_line.trim_end();
        if line.starts_with("- ") {
            if !current.is_empty() {
                items.push(current.join("\n").trim_end().to_string());
                current.clear();
            }
            current.push(line.to_string());
            continue;
        }

        if current.is_empty() {
            continue;
        }

        if line.trim().is_empty() {
            current.push(String::new());
            continue;
        }

        if raw_line.starts_with(' ') || raw_line.starts_with('\t') {
            current.push(line.to_string());
            continue;
        }

        items.push(current.join("\n").trim_end().to_string());
        current.clear();
    }

    if !current.is_empty() {
        items.push(current.join("\n").trim_end().to_string());
    }

    items
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};

    use super::{
        ARCHIVED_HEADER, REVIEW_HEADER, RepoProgress, TrackedRepoState,
        append_reference_repo_clause, collect_tracked_repo_states, discover_sibling_git_repos,
        ensure_review_docs, extract_review_items, resolve_reference_repos, summarize_repo_progress,
    };

    #[test]
    fn extracts_bullet_review_items() {
        let content = "# COMPLETED\n\n- `VAL-001` Added validation\n  Validation:\n  `cargo test`\n\n- `SEC-001` Hardened auth\n  Note: tightened auth boundary\n";
        let items = extract_review_items(content);
        assert_eq!(
            items,
            vec![
                "- `VAL-001` Added validation\n  Validation:\n  `cargo test`".to_string(),
                "- `SEC-001` Hardened auth\n  Note: tightened auth boundary".to_string()
            ]
        );
    }

    #[test]
    fn extracts_section_review_items() {
        let content = "# COMPLETED\n\n## `VAL-001` Added validation\nValidation: pytest\n\n## `SEC-001` Hardened auth\nValidation: ruff check";
        let items = extract_review_items(content);
        assert_eq!(items.len(), 2);
        assert!(items[0].starts_with("## `VAL-001`"));
        assert!(items[1].starts_with("## `SEC-001`"));
    }

    #[test]
    fn initializes_review_and_archived_docs() {
        let temp = unique_temp_dir();
        fs::create_dir_all(&temp).expect("create temp dir");
        let review_path = temp.join("REVIEW.md");
        let archived_path = temp.join("ARCHIVED.md");

        ensure_review_docs(&review_path, &archived_path).expect("init docs");

        assert_eq!(
            fs::read_to_string(review_path).expect("read review"),
            format!("{REVIEW_HEADER}\n\n")
        );
        assert_eq!(
            fs::read_to_string(archived_path).expect("read archived"),
            format!("{ARCHIVED_HEADER}\n\n")
        );

        fs::remove_dir_all(temp).expect("cleanup temp dir");
    }

    #[test]
    fn appends_reference_repo_clause_when_repos_present() {
        let prompt = append_reference_repo_clause(
            "review prompt".to_string(),
            &[PathBuf::from("/home/r/coding/robopokermulti")],
        );

        assert!(prompt.contains("Additional repositories you may inspect or edit"));
        assert!(prompt.contains("/home/r/coding/robopokermulti"));
        assert!(prompt.contains("owned or changed surfaces live in one of these repos"));
    }

    #[test]
    fn discovers_sibling_git_repos_by_default() {
        let workspace = unique_temp_dir();
        let repo_root = workspace.join("bitpoker");
        let sibling_repo = workspace.join("robopokermulti");
        let non_repo = workspace.join("notes");

        init_git_repo(&repo_root);
        init_git_repo(&sibling_repo);
        fs::create_dir_all(&non_repo).expect("failed to create non-repo dir");

        let discovered = discover_sibling_git_repos(&repo_root).expect("discover siblings");
        assert_eq!(
            discovered,
            vec![sibling_repo.canonicalize().expect("canonical sibling")]
        );

        fs::remove_dir_all(&workspace).expect("cleanup workspace");
    }

    #[test]
    fn resolve_reference_repos_merges_siblings_and_explicit_paths() {
        let workspace = unique_temp_dir();
        let repo_root = workspace.join("bitpoker");
        let sibling_repo = workspace.join("robopokermulti");
        let explicit_repo = workspace.join("sharedlib");

        init_git_repo(&repo_root);
        init_git_repo(&sibling_repo);
        init_git_repo(&explicit_repo);

        let resolved = resolve_reference_repos(
            &repo_root,
            &[PathBuf::from("../sharedlib"), sibling_repo.clone()],
        )
        .expect("resolve repos");

        assert_eq!(
            resolved,
            vec![
                sibling_repo.canonicalize().expect("canonical sibling"),
                explicit_repo.canonicalize().expect("canonical explicit"),
            ]
        );

        fs::remove_dir_all(&workspace).expect("cleanup workspace");
    }

    #[test]
    fn repo_progress_detects_reference_repo_commit() {
        let before = vec![
            TrackedRepoState::new("bitpoker", "/tmp/bitpoker", "aaa111", ""),
            TrackedRepoState::new("robopokermulti", "/tmp/robopokermulti", "bbb111", ""),
        ];
        let after = vec![
            TrackedRepoState::new("bitpoker", "/tmp/bitpoker", "aaa111", ""),
            TrackedRepoState::new("robopokermulti", "/tmp/robopokermulti", "bbb222", ""),
        ];

        let progress = summarize_repo_progress(&before, &after);
        assert_eq!(progress, RepoProgress::NewCommits);
    }

    #[test]
    fn repo_progress_flags_dirty_reference_repo_without_commit() {
        let before = vec![
            TrackedRepoState::new("bitpoker", "/tmp/bitpoker", "aaa111", ""),
            TrackedRepoState::new("robopokermulti", "/tmp/robopokermulti", "bbb111", ""),
        ];
        let after = vec![
            TrackedRepoState::new("bitpoker", "/tmp/bitpoker", "aaa111", ""),
            TrackedRepoState::new(
                "robopokermulti",
                "/tmp/robopokermulti",
                "bbb111",
                " M src/lib.rs",
            ),
        ];

        let progress = summarize_repo_progress(&before, &after);
        assert_eq!(
            progress,
            RepoProgress::DirtyChanges(vec!["robopokermulti".to_string()])
        );
    }

    #[test]
    fn collect_tracked_repo_states_skips_unborn_reference_repo() {
        let workspace = unique_temp_dir();
        let repo_root = workspace.join("bitpoker");
        let unborn_reference = workspace.join("hermes-autodev-framework");

        init_git_repo(&repo_root);
        commit_empty_change(&repo_root);
        init_git_repo(&unborn_reference);

        let states =
            collect_tracked_repo_states(&repo_root, std::slice::from_ref(&unborn_reference))
                .expect("collect repo states");

        assert_eq!(states.len(), 1);
        assert_eq!(states[0].path, repo_root);

        fs::remove_dir_all(&workspace).expect("cleanup workspace");
    }

    fn init_git_repo(path: &PathBuf) {
        fs::create_dir_all(path).expect("failed to create repo dir");
        let status = std::process::Command::new("git")
            .args(["init", "-q"])
            .arg(path)
            .status()
            .expect("failed to run git init");
        assert!(status.success(), "git init should succeed");
    }

    fn commit_empty_change(path: &PathBuf) {
        let status = std::process::Command::new("git")
            .args([
                "-c",
                "user.name=Autodev Tests",
                "-c",
                "user.email=autodev-tests@example.com",
                "commit",
                "--allow-empty",
                "-m",
                "initial commit",
            ])
            .current_dir(path)
            .status()
            .expect("failed to run git commit");
        assert!(status.success(), "git commit should succeed");
    }

    fn unique_temp_dir() -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        std::env::temp_dir().join(format!("autodev-review-test-{nanos}"))
    }
}
