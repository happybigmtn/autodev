use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{bail, Context, Result};

use crate::claude_exec::run_claude_exec;
use crate::codex_exec::run_codex_exec;
use crate::util::{
    atomic_write, auto_checkpoint_if_needed, ensure_repo_layout, git_repo_root, git_stdout,
    push_branch_with_remote_sync, repo_name, sync_branch_with_remote, timestamp_slug,
};
use crate::LoopArgs;

const KNOWN_PRIMARY_BRANCHES: [&str; 3] = ["main", "master", "trunk"];

pub(crate) const DEFAULT_LOOP_PROMPT_TEMPLATE: &str = r#"0a. Study `AGENTS.md` for repo-specific build, validation, and staging rules.
0b. Study `IMPLEMENTATION_PLAN.md` and identify the first unchecked task whose explicit dependencies are already satisfied.
0c. Study `specs/*` with full repo context, but when multiple dated specs cover the same surface, treat the newest spec referenced by the current unchecked task as authoritative. Older or duplicate specs are historical context only.
0d. Use the specs, plan, and the live codebase as a single contract. If they disagree, treat the code and the current task's authoritative specs as evidence, record the conflict truthfully, and do not bluff your way through it.
0e. For every current-state fact, trust the live codebase over planning artifacts unless the code is plainly stale and the repo includes stronger primary-source evidence.
0f. When additional repositories are listed below, inspect and edit them directly when the current task's owned surfaces, acceptance criteria, or blocker evidence point there. Read each touched repo's own `AGENTS.md` and operational docs before editing it.

1. Your task is to implement functionality per the specifications using the full repository context.
   - Follow `IMPLEMENTATION_PLAN.md` in order and take the next unchecked task from top to bottom.
   - Do not reprioritize the queue yourself.
   - Before making changes, search the codebase, tests, and planning artifacts. Do not assume a surface is missing until you verify it.
   - If the current task's owned surfaces live in an additional listed repo, do the code change there while keeping this queue repo's planning artifacts truthful.
   - Build a short task brief for yourself before editing: task id, spec refs, owned surfaces, integration touchpoints, scope boundary, acceptance criteria, verification, and any assumptions you are relying on.
   - Restate the task's assumptions and success conditions from repo evidence before editing. If the plan/spec/task contract is ambiguous, resolve the ambiguity in the docs before pretending implementation can start.

2. Implement the task in the smallest truthful slice that fully closes it using a RED/GREEN/REFACTOR cycle by default:
   - Stay within the task contract's owned surfaces plus the minimum adjacent integration edits needed to make the code work.
   - Prefer the simplest solution that matches the existing codebase patterns. Do not add abstractions that are not earning their complexity.
   - Keep the codebase compilable while you work. Do not leave placeholders, TODOs, or half-wired scaffolding.
   - If the repo is still greenfield, perform the bootstrap work the plan requires instead of pretending later tasks are ready.
   - If the task changes behavior or fixes a bug, start by writing or identifying a failing test, failing command, or other executable proof. Confirm the proof fails before claiming the bug or missing behavior is reproduced.
   - Make the minimum code change that turns the proof green.
   - After the proof is green, run a short simplification pass on the touched code: improve names, remove dead paths, reduce unnecessary branching, and collapse unearned abstractions without changing behavior or widening scope.
   - For browser-facing or runtime-sensitive changes, use browser/runtime verification when available instead of relying on static reasoning alone.
   - If the slice needs to land before the full user-facing feature is ready, prefer existing safe-default or feature-gating patterns in the repo. Do not invent a new flag system if the repo has none.

3. When anything breaks, stop the line and debug systematically:
   - Preserve the failing command, output, repro step, or screenshot evidence.
   - Reproduce the failure as narrowly as you can.
   - Fix the root cause, not the nearest symptom.
   - Guard against recurrence with tests or tighter validation when practical.
   - Resume feature work only after the task's verification story is truthful again.

4. Keep the planning artifacts current:
   - When you discover important implementation facts, blockers, or scope corrections, update `IMPLEMENTATION_PLAN.md`.
   - When you finish a task, remove its entry from `IMPLEMENTATION_PLAN.md` so the plan remains an active queue of unfinished work only.
   - Append a concise record to `COMPLETED.md` with task id, what was completed, the validation command(s), and commit sha.
   - If you notice worthwhile out-of-scope work, append a concise item to `WORKLIST.md` instead of quietly broadening scope.
   - Update `AGENTS.md` only when you learn something operational that will help future loops run or validate the repo correctly.

5. When validation passes, commit the increment:
   - Stage only the files relevant to the completed task plus `IMPLEMENTATION_PLAN.md`, `COMPLETED.md`, `WORKLIST.md`, and `AGENTS.md` when they changed.
   - Do not sweep unrelated pre-existing churn into the commit.
   - If you touch multiple repositories, commit and push each repository separately. Never try to mix files from different git repos into one commit.
   - Commit with a message like `repo-name: TASK-ID short description` using the actual repository name for each touched repo.
   - Before committing, rerun the task's direct proof plus the strongest broad regression commands this repo honestly supports.
   - After committing, run `git status` in every touched repo to verify no implementation files were left unstaged. If any were, amend the relevant commit.
   - Push the queue repo directly to `origin/{branch}` after the commit. For additional listed repos, push the currently checked-out branch unless that repo's own instructions require something else.

6. If you hit a real blocker after genuine debugging:
   - Record the blocker under the task in `IMPLEMENTATION_PLAN.md`.
   - Commit the planning update if it materially changes the execution record.
   - Move to the next ready task instead of repeating the same failed attempt.

7. Task-order rule:
   - Treat the order in `IMPLEMENTATION_PLAN.md` as authoritative.
   - Work on the first unchecked task unless its explicit dependencies are still unchecked.
   - If the current task is already satisfied, remove it from `IMPLEMENTATION_PLAN.md`, append a truthful note to `COMPLETED.md`, and continue downward.

8. Branch rule:
   - Work only on branch `{branch}`.
   - Do not create or push feature branches, lane branches, or topic branches.

99999. Important: keep `AGENTS.md` operational only.
999999. Important: prefer complete working increments over placeholders.
9999999. Important: if unrelated tests fail and they prevent a truthful green result, fix them as part of the increment.
99999999. CRITICAL: Do not assume functionality is missing — search the codebase to confirm before implementing anything new.
999999999. Every new module must be importable and wired into the package. Dead code that isn't reachable from any entry point is an island — wire it before committing.
9999999999. When you learn something new about how to build, run, or validate the repo, update `AGENTS.md` — but keep it brief and operational only.
99999999999. A task is not done because the code looks right. It is done when the acceptance criteria are satisfied and the verification evidence is real."#;

pub(crate) async fn run_loop(args: LoopArgs) -> Result<()> {
    let repo_root = git_repo_root()?;
    ensure_repo_layout(&repo_root)?;
    let reference_repos = resolve_reference_repos(&repo_root, &args.reference_repos)?;

    let current_branch = git_stdout(&repo_root, ["branch", "--show-current"])?;
    let current_branch = current_branch.trim().to_string();
    let target_branch = resolve_loop_branch(&repo_root, args.branch.as_deref(), &current_branch)?;
    if current_branch != target_branch {
        bail!(
            "auto loop must run on branch `{}` (current: `{}`)",
            target_branch,
            current_branch
        );
    }

    let prompt_template = match &args.prompt_file {
        Some(path) => {
            let prompt = fs::read_to_string(path)
                .with_context(|| format!("failed to read prompt file {}", path.display()))?;
            append_reference_repo_clause(prompt, &reference_repos)
        }
        None => render_default_loop_prompt(&target_branch, &reference_repos),
    };
    let full_prompt = format!("{prompt_template}\n\nExecute the instructions above.");

    let run_root = args
        .run_root
        .unwrap_or_else(|| repo_root.join(".auto").join("loop"));
    fs::create_dir_all(&run_root)
        .with_context(|| format!("failed to create {}", run_root.display()))?;
    let stderr_log_path = run_root.join("codex.stderr.log");

    let harness = if args.claude { "Claude" } else { "Codex" };

    println!("auto loop");
    println!("repo root:   {}", repo_root.display());
    println!("branch:      {}", target_branch);
    if args.claude {
        println!("harness:     Claude (Opus 4.6 high)");
        println!("max turns:   {}", args.max_turns);
    } else {
        println!("model:       {}", args.model);
        println!("reasoning:   {}", args.reasoning_effort);
    }
    println!("run root:    {}", run_root.display());
    if !reference_repos.is_empty() {
        println!("references:  {}", reference_repos.len());
        for path in &reference_repos {
            println!("  - {}", path.display());
        }
    }
    println!(
        "prompt:      {}",
        args.prompt_file
            .as_ref()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|| "built-in Ralph worker".to_string())
    );

    if sync_branch_with_remote(&repo_root, target_branch.as_str())? {
        println!("remote sync: rebased onto origin/{}", target_branch);
    }

    if let Some(commit) =
        auto_checkpoint_if_needed(&repo_root, target_branch.as_str(), "auto loop checkpoint")?
    {
        println!("checkpoint:  committed pre-existing changes at {commit}");
    }

    let mut iteration = 0usize;
    loop {
        if args.max_iterations.is_some_and(|limit| iteration >= limit) {
            println!(
                "reached max iterations: {}",
                args.max_iterations.unwrap_or_default()
            );
            break;
        }

        let prompt_path = repo_root
            .join(".auto")
            .join("logs")
            .join(format!("loop-{}-prompt.md", timestamp_slug()));
        atomic_write(&prompt_path, full_prompt.as_bytes())
            .with_context(|| format!("failed to write {}", prompt_path.display()))?;
        println!("prompt log:  {}", prompt_path.display());

        let state_before = collect_tracked_repo_states(&repo_root, &reference_repos)?;
        println!();
        println!("running {harness} iteration {}", iteration + 1);

        let exit_status = if args.claude {
            run_claude_exec(
                &repo_root,
                &full_prompt,
                args.max_turns,
                &stderr_log_path,
                "auto loop",
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
                "auto loop",
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
        println!("{harness} iteration complete");

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
                    target_branch.as_str(),
                    "auto loop checkpoint",
                )? {
                    iteration += 1;
                    println!("checkpoint:  committed iteration changes at {commit}");
                    println!();
                    println!("================ LOOP {} ================", iteration);
                    continue;
                }
                println!("no new commit detected; stopping.");
                break;
            }
        }

        if push_branch_with_remote_sync(&repo_root, target_branch.as_str())? {
            println!("remote sync: rebased onto origin/{}", target_branch);
        }
        if let Some(commit) =
            auto_checkpoint_if_needed(&repo_root, target_branch.as_str(), "auto loop checkpoint")?
        {
            println!("checkpoint:  committed trailing changes at {commit}");
        }
        iteration += 1;
        println!();
        println!("================ LOOP {} ================", iteration);
    }

    Ok(())
}

fn render_default_loop_prompt(branch: &str, reference_repos: &[PathBuf]) -> String {
    append_reference_repo_clause(
        DEFAULT_LOOP_PROMPT_TEMPLATE.replace("{branch}", branch),
        reference_repos,
    )
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
        "{prompt}\n\nAdditional repositories you may inspect or edit when the task contract points there:\n{listing}\n\nRepository-crossing rules:\n- If the current task's owned surfaces live in one of these repos, implement the code change there instead of pretending the queue repo should own it.\n- Keep `IMPLEMENTATION_PLAN.md` truthful as the active queue for this run even when code lands in another repo.\n- Read each touched repo's `AGENTS.md`, tests, and operational docs before editing it.\n- Commit and push each touched repo separately.\n"
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
            name: repo_name(&path),
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

fn resolve_loop_branch(
    repo_root: &Path,
    requested_branch: Option<&str>,
    current_branch: &str,
) -> Result<String> {
    let origin_head = git_stdout(
        repo_root,
        [
            "symbolic-ref",
            "--quiet",
            "--short",
            "refs/remotes/origin/HEAD",
        ],
    )
    .ok();
    let available = KNOWN_PRIMARY_BRANCHES
        .into_iter()
        .filter(|candidate| git_branch_exists(repo_root, candidate))
        .collect::<Vec<_>>();
    pick_loop_branch(
        requested_branch,
        current_branch,
        origin_head.as_deref(),
        &available,
    )
}

fn pick_loop_branch(
    requested_branch: Option<&str>,
    current_branch: &str,
    origin_head: Option<&str>,
    available_primary_branches: &[&str],
) -> Result<String> {
    if let Some(branch) = requested_branch
        .map(str::trim)
        .filter(|branch| !branch.is_empty())
    {
        return Ok(branch.to_string());
    }

    if is_primary_branch_name(current_branch) {
        return Ok(current_branch.to_string());
    }

    if let Some(branch) = origin_head.and_then(parse_origin_head_branch) {
        return Ok(branch);
    }

    if let Some(branch) = KNOWN_PRIMARY_BRANCHES
        .into_iter()
        .find(|candidate| available_primary_branches.contains(candidate))
    {
        return Ok(branch.to_string());
    }

    bail!(
        "auto loop could not resolve the repo's primary branch; pass `--branch <name>` explicitly"
    );
}

fn parse_origin_head_branch(origin_head: &str) -> Option<String> {
    let trimmed = origin_head.trim();
    let branch = trimmed.strip_prefix("origin/").unwrap_or(trimmed).trim();
    (!branch.is_empty()).then(|| branch.to_string())
}

fn is_primary_branch_name(branch: &str) -> bool {
    KNOWN_PRIMARY_BRANCHES.contains(&branch.trim())
}

fn git_branch_exists(repo_root: &Path, branch: &str) -> bool {
    git_ref_exists(repo_root, &format!("refs/heads/{branch}"))
        || git_ref_exists(repo_root, &format!("refs/remotes/origin/{branch}"))
}

fn git_ref_exists(repo_root: &Path, git_ref: &str) -> bool {
    Command::new("git")
        .arg("-C")
        .arg(repo_root)
        .args(["show-ref", "--verify", "--quiet", git_ref])
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::PathBuf;
    use std::process::Command;
    use std::time::{SystemTime, UNIX_EPOCH};

    use super::{
        discover_sibling_git_repos, parse_origin_head_branch, pick_loop_branch,
        render_default_loop_prompt, resolve_reference_repos, summarize_repo_progress, RepoProgress,
        TrackedRepoState,
    };

    #[test]
    fn default_prompt_uses_resolved_branch() {
        let prompt = render_default_loop_prompt("trunk", &[]);
        assert!(prompt.contains("origin/trunk"));
        assert!(prompt.contains("branch `trunk`"));
        assert!(!prompt.contains("origin/main"));
        assert!(prompt.contains("RED/GREEN/REFACTOR"));
        assert!(prompt.contains("failing test"));
        assert!(prompt.contains("simplification pass"));
        assert!(prompt.contains("newest spec referenced by the current unchecked task"));
        assert!(prompt.contains("historical context only"));
    }

    #[test]
    fn default_prompt_lists_reference_repos_when_declared() {
        let prompt =
            render_default_loop_prompt("main", &[PathBuf::from("/home/r/coding/robopokermulti")]);
        assert!(prompt.contains("Additional repositories you may inspect or edit"));
        assert!(prompt.contains("/home/r/coding/robopokermulti"));
        assert!(prompt.contains("owned surfaces live in one of these repos"));
    }

    #[test]
    fn branch_picker_prefers_explicit_branch() {
        let branch =
            pick_loop_branch(Some("release"), "main", Some("origin/trunk"), &["trunk"]).unwrap();
        assert_eq!(branch, "release");
    }

    #[test]
    fn branch_picker_uses_origin_head_when_available() {
        let branch = pick_loop_branch(None, "feature/test", Some("origin/master"), &[]).unwrap();
        assert_eq!(branch, "master");
    }

    #[test]
    fn branch_picker_prefers_current_primary_branch_over_origin_head() {
        let branch =
            pick_loop_branch(None, "main", Some("origin/master"), &["main", "master"]).unwrap();
        assert_eq!(branch, "main");
    }

    #[test]
    fn branch_picker_falls_back_to_current_primary_branch() {
        let branch = pick_loop_branch(None, "trunk", None, &[]).unwrap();
        assert_eq!(branch, "trunk");
    }

    #[test]
    fn branch_picker_falls_back_to_known_available_branch() {
        let branch = pick_loop_branch(None, "feature/test", None, &["master"]).unwrap();
        assert_eq!(branch, "master");
    }

    #[test]
    fn parses_origin_head_branch() {
        assert_eq!(
            parse_origin_head_branch("origin/trunk"),
            Some("trunk".to_string())
        );
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
    fn discovers_sibling_git_repos_by_default() {
        let workspace = unique_temp_dir("loop-siblings");
        let repo_root = workspace.join("bitpoker");
        let sibling_repo = workspace.join("robopokermulti");
        let non_repo = workspace.join("notes");

        init_git_repo(&repo_root);
        init_git_repo(&sibling_repo);
        fs::create_dir_all(&non_repo).expect("failed to create non-repo dir");

        let discovered = discover_sibling_git_repos(&repo_root).expect("should discover siblings");

        assert_eq!(
            discovered,
            vec![sibling_repo.canonicalize().expect("canonical sibling")]
        );

        fs::remove_dir_all(&workspace).expect("failed to remove temp workspace");
    }

    #[test]
    fn resolve_reference_repos_merges_siblings_and_explicit_paths() {
        let workspace = unique_temp_dir("loop-reference-merge");
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
        .expect("should resolve sibling and explicit repos");

        assert_eq!(
            resolved,
            vec![
                sibling_repo.canonicalize().expect("canonical sibling"),
                explicit_repo.canonicalize().expect("canonical explicit"),
            ]
        );

        fs::remove_dir_all(&workspace).expect("failed to remove temp workspace");
    }

    fn init_git_repo(path: &PathBuf) {
        fs::create_dir_all(path).expect("failed to create repo dir");
        let status = Command::new("git")
            .args(["init", "-q"])
            .arg(path)
            .status()
            .expect("failed to run git init");
        assert!(status.success(), "git init should succeed");
    }

    fn unique_temp_dir(label: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("time went backwards")
            .as_nanos();
        std::env::temp_dir().join(format!("autodev-{label}-{nanos}"))
    }
}
