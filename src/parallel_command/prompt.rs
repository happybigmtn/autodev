use super::*;

pub(crate) fn build_parallel_lane_prompt(
    prompt_template: &str,
    plan: &LoopPlanSnapshot,
    task: &LoopTask,
    branch: &str,
    cargo_target_clause: &str,
    preflight_clause: &str,
    host_recovery_note: Option<&str>,
) -> String {
    let queue = plan.queue_snapshot();
    let blocked_clause = if queue.blocked_ids.is_empty() {
        "none".to_string()
    } else {
        queue.blocked_ids.join(", ")
    };
    let dependency_clause = if task.dependencies.is_empty() {
        "none".to_string()
    } else {
        task.dependencies.join(", ")
    };
    let protected_files = HOST_QUEUE_STATE_FILES
        .into_iter()
        .map(|file| format!("`{file}`"))
        .collect::<Vec<_>>()
        .join(", ");
    let protected_clause = format!(
        "Do not edit these shared queue files in this lane. The host owns queue reconciliation in parallel mode: {}.",
        protected_files
    );
    let recovery_clause = host_recovery_note
        .map(str::trim)
        .filter(|note| !note.is_empty())
        .map(|note| format!("\nHost recovery context:\n{note}\n"))
        .unwrap_or_default();
    let preflight_clause = preflight_clause
        .trim()
        .is_empty()
        .then(String::new)
        .unwrap_or_else(|| format!("\nHost preflight report:\n{}\n", preflight_clause.trim()));
    let verification = verification_plan(&task.markdown);
    let verification_commands_clause = if verification.executable_commands.is_empty() {
        "none parsed".to_string()
    } else {
        verification
            .executable_commands
            .iter()
            .map(|command| format!("`{command}`"))
            .collect::<Vec<_>>()
            .join(", ")
    };
    let verification_guidance_clause = if verification.narrative_guidance.is_empty() {
        "none".to_string()
    } else {
        verification.narrative_guidance.join(" | ")
    };
    let remote_git_policy_clause = remote_git_policy_clause(
        env::var_os("AUTO_SKIP_REMOTE_SYNC").is_some_and(|value| !value.as_os_str().is_empty()),
        branch,
    );
    format!(
        "{prompt_template}\n\nParallel assignment for this worker:\n- Assigned task for this lane: `{task_id}` {title}\n- This task is already dependency-ready for this run: {dependency_clause}\n- The host owns queue reconciliation and branch landing in parallel mode.\n- {remote_git_policy_clause}\n- Before finishing, run `git status --short`. Finish only with at least one local commit for this task and a clean worktree. If files are still dirty, either commit task-owned leftovers or revert unrelated/formatter spillover before exiting.\n- {protected_clause}\n- {cargo_target_clause}\n- If the repo contains `scripts/run-task-verification.sh`, run the host-parsed executable verification commands through that wrapper instead of invoking them bare. For simple commands, pass normal argv after `--`, for example `scripts/run-task-verification.sh {task_id} -- npm run typecheck`. For pipes, `!`, `||`, redirection, env setup, or other shell syntax, use `bash -lc`, for example `scripts/run-task-verification.sh {task_id} -- bash -lc 'rg -n \"foo|bar\" src || true'`. Do not pass a whole shell command as one bare quoted string after `--`; the wrapper executes argv directly. Do not treat narrative `Verification:` prose as literal shell input.\n- Host-parsed executable verification commands: {verification_commands_clause}\n- Narrative verification guidance preserved from the task: {verification_guidance_clause}\n- Source-of-truth discipline: runtime/engine/API owners define facts; UI/presentation code renders those facts. Do not duplicate runtime-owned catalogs, constants, settlement math, risk classifications, eligibility rules, balances, or status derivations in UI code.\n- Runtime-first order: when the task touches both runtime and UI, implement or confirm the runtime/API contract first, regenerate/check generated bindings or schemas second, then update UI consumers.\n- Fixture boundary: production code must not import fixture/demo/sample data as fallback truth. Fixture data belongs in tests, stories, demos, or explicit dev-only harnesses.\n- Contract generation: if the task names generated artifacts or changes runtime/API shapes, run the named generator/check or record `AUTO_ENV_BLOCKER`/`AUTO_VERIFICATION_BLOCKER` with the exact reason it could not run.\n- Cross-surface proof: if UI consumers are named, include at least one runtime-output-to-UI/readback proof or a clear blocker. Component-only tests are insufficient when the original risk is runtime/UI drift.\n- Retire-first cleanup: if the task names retired or superseded surfaces, delete/archive/tombstone them and clean callers/indexes in the same lane when in scope. Do not leave stale active doctrine as a TODO unless the task explicitly gates it.\n- Independent closeout: before your final answer, re-check the original task fields (`Source of truth`, `Runtime owner`, `UI consumers`, `Generated artifacts`, `Fixture boundary`, `Retired surfaces`, and `Review/closeout`) and state how each was satisfied or blocked.\n- If no executable verification commands were parsed, derive the narrowest truthful proof yourself and record blockers honestly instead of patching the wrapper to accept prose.\n- If a proof command exits successfully but reports `0 tests`, treat that proof as not run. Find the exact test/package target or report the verification blocker; do not count zero-test output as passing evidence.\n- Do not use direct target-dir test binaries as final proof unless you built that exact artifact from this lane's current source tree in the immediately preceding command. Prefer `cargo test` or the repo's verification wrapper.\n- If missing external infrastructure blocks verification or runtime smoke tests, print `AUTO_ENV_BLOCKER: <short reason>` before exiting non-zero. Do not present an environment blocker as a code proof failure.\n- Never hand-edit or delete `.auto/symphony/verification-receipts/*.json` and never `git add`/commit them. The wrapper writes them; leave them in the worktree exactly as written. The host will propagate them to canonical after harvest and then embed durable proof in its own closeout commit footer. If you remove them locally, the host cannot promote the task to `[x]`.\n- The host marks this task `- [x]` only when local review handoff, verification evidence, and declared completion artifacts are present. Otherwise it leaves the task `- [~]` for follow-up instead of bluffing completion.
- Already-complete check: If you discover the task is genuinely already complete -- all acceptance criteria match the current main tree, all executable verification commands pass, declared completion artifacts exist, and you would not need to add or change any non-doc source file to satisfy the task -- do not edit IMPLEMENTATION_PLAN.md yourself. Print `AUTO_ALREADY_COMPLETE: {task_id} <proof command/artifact>` in your final answer and leave host reconciliation to the host. Do NOT use this to bluff completion when work is missing; only when the work was provably done in a prior pass and no new code is needed.\n{preflight_clause}{recovery_clause}\nCanonical queue snapshot when this lane started:\n- Unfinished task count: {pending_count}\n- Currently blocked tasks: {blocked_clause}\n\nAssigned task markdown:\n{markdown}\n",
        task_id = task.id,
        title = task.title,
        dependency_clause = dependency_clause,
        remote_git_policy_clause = remote_git_policy_clause,
        protected_clause = protected_clause,
        cargo_target_clause = cargo_target_clause,
        verification_commands_clause = verification_commands_clause,
        verification_guidance_clause = verification_guidance_clause,
        preflight_clause = preflight_clause,
        recovery_clause = recovery_clause,
        pending_count = queue.pending_ids.len(),
        blocked_clause = blocked_clause,
        markdown = task.markdown
    )
}

fn remote_git_policy_clause(skip_remote_sync: bool, branch: &str) -> String {
    let mut clause = format!(
        "Never run remote-mutating or remote-sync git commands in a lane: no `git push`, `git pull`, `git fetch`, or `git rebase`. Do not probe whether push/fetch would work. Create local commit(s) only; the host will land and sync them onto `{branch}`. If a remote command seems necessary, stop and print `AUTO_ENV_BLOCKER: lane requires host remote sync`."
    );
    if skip_remote_sync {
        clause.push_str(
            " Local-only mode is active because `AUTO_SKIP_REMOTE_SYNC=1` was set when the host launched this lane: there may be no `origin`, and any `git push`, `git pull`, `git fetch`, or `git rebase` attempt fails the whole lane even after valid local work. After your local commit and `git status --short`, print the final handoff and stop.",
        );
    }
    clause
}

pub(crate) fn render_default_parallel_prompt(branch: &str, reference_repos: &[PathBuf]) -> String {
    let base = crate::loop_command::DEFAULT_LOOP_PROMPT_TEMPLATE.replace("{branch}", branch);
    let parallel_guard = "\n\nParallel execution guard:\n- This lane implements exactly the assigned task. Do not maintain planning ledgers, write audits, or create report artifacts unless the assigned task explicitly owns them.\n- Done means an observable product or behavior change covered by a passing test. Receipts, ledgers, evidence rollups, and closeout notes are automatic side-effects of that change, never the deliverable; a task closed by writing a guard, audit, receipt, or doc without changing product behavior is NOT done -- find the behavior it was meant to change. Preserve completion evidence in code, tests, generated receipts, and final stdout. The host owns queue updates, review handoff, and status reconciliation.\n- Prefer the smallest truthful source/test/runtime/UX proof that closes the task over bonus documentation or artifact volume.\n";
    append_reference_repo_clause(format!("{base}{parallel_guard}"), reference_repos)
}

pub(crate) fn repo_forbids_legacy_review_trackers(repo_root: &Path) -> bool {
    ["AGENTS.md", "WORKFLOW.md"].iter().any(|relative| {
        fs::read_to_string(repo_root.join(relative)).is_ok_and(|content| {
            content.contains("Do not restore")
                && content.contains("COMPLETED.md")
                && content.contains("WORKLIST.md")
                && content.contains("ARCHIVED.md")
                && content.contains("REVIEW.md")
        })
    })
}

pub(crate) fn append_reference_repo_clause(prompt: String, reference_repos: &[PathBuf]) -> String {
    if reference_repos.is_empty() {
        return prompt;
    }

    let listing = reference_repos
        .iter()
        .map(|path| format!("- `{}`", path.display()))
        .collect::<Vec<_>>()
        .join("\n");
    format!(
        "{prompt}\n\nAdditional repositories you may inspect as read-only context:\n{listing}\n\nRepository-crossing rules:\n- Treat every additional repository as read-only. Do not edit, format, stage, commit, push, or run mutating generators in those repos.\n- Implement only the assigned repo's owned surfaces from this lane. If the current task needs code changes in a reference repo, leave a precise follow-up plan item or blocker instead of writing through another repo's canonical worktree.\n- You may read a reference repo's `AGENTS.md`, tests, and operational docs to verify contracts and shape local adapters or fixtures.\n"
    )
}

pub(crate) fn resolve_reference_repos(
    repo_root: &Path,
    paths: &[PathBuf],
    include_siblings: bool,
) -> Result<Vec<PathBuf>> {
    let mut resolved = if include_siblings {
        discover_sibling_git_repos(repo_root)?
    } else {
        Vec::new()
    };
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

pub(crate) fn discover_sibling_git_repos(repo_root: &Path) -> Result<Vec<PathBuf>> {
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
pub(crate) struct TrackedRepoState {
    pub(crate) name: String,
    pub(crate) path: PathBuf,
    pub(crate) head: String,
    pub(crate) status: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum RepoProgress {
    None,
    NewCommits,
    DirtyChanges(Vec<String>),
}

pub(crate) fn collect_tracked_repo_states(
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
        let status = git_status_short_filtered(&path).unwrap_or_default();
        states.push(TrackedRepoState {
            name: repo_name(&path),
            path,
            head: head.trim().to_string(),
            status: status.trim().to_string(),
        });
    }
    Ok(states)
}

pub(crate) fn summarize_repo_progress(
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

pub(crate) fn resolve_loop_branch(
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

pub(crate) fn pick_loop_branch(
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
        "auto parallel could not resolve the repo's primary branch; pass `--branch <name>` explicitly"
    );
}

pub(crate) fn parse_origin_head_branch(origin_head: &str) -> Option<String> {
    let trimmed = origin_head.trim();
    let branch = trimmed.strip_prefix("origin/").unwrap_or(trimmed).trim();
    (!branch.is_empty()).then(|| branch.to_string())
}

pub(crate) fn is_primary_branch_name(branch: &str) -> bool {
    KNOWN_PRIMARY_BRANCHES.contains(&branch.trim())
}

#[cfg(test)]
mod tests {
    use crate::parallel_command::*;
    use std::time::UNIX_EPOCH;

    fn init_git_repo(path: &PathBuf) {
        fs::create_dir_all(path).expect("failed to create repo dir");
        let status = Command::new("git")
            .args(["init", "-q"])
            .arg(path)
            .status()
            .expect("failed to run git init");
        assert!(status.success(), "git init should succeed");
        git_ok(path, ["config", "user.email", "test@example.com"]);
        git_ok(path, ["config", "user.name", "Autodev Test"]);
    }

    fn git_ok<const N: usize>(repo: &PathBuf, args: [&str; N]) {
        let output = Command::new("git")
            .arg("-C")
            .arg(repo)
            .args(args)
            .output()
            .expect("failed to run git");
        assert!(
            output.status.success(),
            "git command failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    fn unique_temp_dir(label: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("time went backwards")
            .as_nanos();
        std::env::temp_dir().join(format!("autodev-{label}-{nanos}"))
    }

    #[test]
    fn default_prompt_uses_resolved_branch() {
        let prompt = render_default_parallel_prompt("trunk", &[]);
        assert!(prompt.contains("branch `trunk`"));
        assert!(!prompt.contains("origin/main"));
        assert!(prompt.contains("Study `AGENTS.md` for repo-specific build"));
        assert!(prompt.contains("RED/GREEN/REFACTOR"));
        assert!(prompt.contains("failing test"));
        assert!(prompt
            .contains("identify the first actionable unfinished task marked `- [ ]` or `- [~]`"));
        assert!(prompt.contains("historical context only"));
        assert!(prompt.contains("first actionable unfinished `- [ ]` or `- [~]` task"));
        assert!(prompt.contains("Completion path: <TASK-ID>"));
        assert!(prompt.contains("mark it `- [x]` only when local verification, review handoff, and required completion artifacts are actually in place"));
        assert!(prompt.contains("Parallel execution guard"));
        assert!(prompt.contains("Do not maintain planning ledgers, write audits, or create report artifacts unless the assigned task explicitly owns them"));
    }

    #[test]
    fn default_prompt_lists_reference_repos_when_declared() {
        let prompt =
            render_default_parallel_prompt("main", &[PathBuf::from("/tmp/robopokermulti")]);
        assert!(prompt.contains("Additional repositories you may inspect as read-only context"));
        assert!(prompt.contains("/tmp/robopokermulti"));
        assert!(prompt.contains("Do not edit, format, stage, commit, push"));
        assert!(prompt.contains("leave a precise follow-up plan item or blocker"));
    }

    #[test]
    fn lane_prompt_requires_clean_committed_finish_and_can_include_recovery_context() {
        let snapshot = parse_loop_plan(
            r#"- [ ] `TASK-001` First task
  Dependencies:
  - None
  Estimated scope: small
"#,
        );
        let task = snapshot.tasks.first().expect("task should parse");
        let prompt = build_parallel_lane_prompt(
            "base prompt",
            &snapshot,
            task,
            "trunk",
            "Use the host-provided `CARGO_TARGET_DIR`; this run gives each lane its own target directory.",
            "- warn agent-browser: daemon missing",
            Some("Resolve the previous landing conflict."),
        );

        assert!(prompt.contains("run `git status --short`"));
        assert!(prompt.contains("at least one local commit for this task and a clean worktree"));
        assert!(prompt.contains("Never run remote-mutating or remote-sync git commands"));
        assert!(prompt.contains("no `git push`, `git pull`, `git fetch`, or `git rebase`"));
        assert!(prompt.contains("AUTO_ENV_BLOCKER: lane requires host remote sync"));
        assert!(prompt.contains("reports `0 tests`"));
        assert!(prompt.contains("direct target-dir test binaries"));
        assert!(prompt.contains("AUTO_ENV_BLOCKER"));
        assert!(prompt.contains("Host-parsed executable verification commands"));
        assert!(
            prompt.contains("Do not treat narrative `Verification:` prose as literal shell input")
        );
        assert!(prompt.contains("For simple commands, pass normal argv after `--`"));
        assert!(prompt.contains("For pipes, `!`, `||`, redirection"));
        assert!(prompt.contains("Do not pass a whole shell command as one bare quoted string"));
        assert!(prompt.contains("Host preflight report:"));
        assert!(prompt.contains("Host recovery context:"));
        assert!(prompt.contains("Resolve the previous landing conflict."));
        assert!(prompt.contains("AUTO_ALREADY_COMPLETE: TASK-001"));
        assert!(!prompt.contains("plan-status-update editing IMPLEMENTATION_PLAN.md"));
    }

    #[test]
    fn remote_git_policy_names_local_only_skip_mode() {
        let clause = super::remote_git_policy_clause(true, "pilot/no-origin");
        assert!(clause.contains("`AUTO_SKIP_REMOTE_SYNC=1`"));
        assert!(clause.contains("there may be no `origin`"));
        assert!(clause.contains("fails the whole lane even after valid local work"));
        assert!(clause.contains("print the final handoff and stop"));
        assert!(clause.contains("`pilot/no-origin`"));
    }

    #[test]
    fn worker_prompt_lists_host_owned_queue_files() {
        let snapshot = parse_loop_plan(
            r#"- [ ] `TASK-001` First task
  Verification: `cargo test task_one`
  Required tests: `cargo test task_one`
  Dependencies: none
"#,
        );
        let task = snapshot.tasks.first().expect("task should parse");
        let prompt = build_parallel_lane_prompt(
            "base prompt",
            &snapshot,
            task,
            "main",
            "Use the host-provided `CARGO_TARGET_DIR`; this run gives each lane its own target directory.",
            "",
            None,
        );

        for file in HOST_QUEUE_STATE_FILES {
            assert!(
                prompt.contains(&format!("`{file}`")),
                "prompt should list host-owned queue file {file}"
            );
        }
        assert!(prompt.contains("The host owns queue reconciliation in parallel mode"));
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
            true,
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
}
