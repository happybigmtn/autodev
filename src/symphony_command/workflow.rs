//! Symphony WORKFLOW.md rendering, config resolution, and the foreground runner.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Stdio;

use anyhow::{bail, Context, Result};
use dirs::cache_dir;
use tokio::process::Command as TokioCommand;

use crate::symphony_command::sync::run_sync;
use crate::util::{atomic_write, git_repo_root, git_stdout, repo_name};
use crate::{SymphonyRunArgs, SymphonySyncArgs, SymphonyWorkflowArgs};

pub(crate) const SYMPHONY_ROOT_ENV: &str = "AUTODEV_SYMPHONY_ROOT";

pub(crate) struct RenderedWorkflow {
    pub(crate) output_path: PathBuf,
    pub(crate) base_branch: String,
    pub(crate) workspace_root: PathBuf,
    logs_root: PathBuf,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
struct WorkflowBootstrapConfig {
    project_slug: Option<String>,
}

pub(crate) async fn render_workflow(args: SymphonyWorkflowArgs) -> Result<RenderedWorkflow> {
    let repo_root = resolve_repo_root(args.repo_root)?;
    let project_slug = resolve_project_slug(&repo_root, args.project_slug.as_deref())?;
    let base_branch = resolve_base_branch(&repo_root, args.base_branch)?;
    let workflow_path = resolve_workflow_path(&repo_root, args.output);
    let workspace_root = resolve_workspace_root(&repo_root, args.workspace_root)?;
    let logs_root = default_logs_root(&repo_root);
    let remote_url = git_stdout(&repo_root, ["remote", "get-url", "origin"])?
        .trim()
        .to_string();
    let repo_label = repo_name(&repo_root);
    let output = render_workflow_markdown(WorkflowRenderSpec {
        repo_root: &repo_root,
        repo_label: &repo_label,
        project_slug: &project_slug,
        remote_url: &remote_url,
        base_branch: &base_branch,
        workspace_root: &workspace_root,
        poll_interval_ms: args.poll_interval_ms,
        max_concurrent_agents: args.max_concurrent_agents,
        model: &args.model,
        reasoning_effort: &args.reasoning_effort,
        todo_state: "Todo",
        in_progress_state: &args.in_progress_state,
        done_state: &args.done_state,
        blocked_state: args.blocked_state.as_deref(),
    })?;

    if let Some(parent) = workflow_path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }
    atomic_write(&workflow_path, output.as_bytes())?;

    Ok(RenderedWorkflow {
        output_path: workflow_path,
        base_branch,
        workspace_root,
        logs_root,
    })
}

pub(crate) async fn run_foreground(args: SymphonyRunArgs) -> Result<()> {
    let symphony_root = resolve_symphony_root(args.symphony_root.clone())?;

    if args.sync_first {
        run_sync(SymphonySyncArgs {
            repo_root: args.repo_root.clone(),
            project_slug: args.project_slug.clone(),
            todo_state: args.todo_state.clone(),
            planner_model: args.planner_model.clone(),
            planner_reasoning_effort: args.planner_reasoning_effort.clone(),
            codex_bin: args.codex_bin.clone(),
            no_ai_planner: args.no_ai_planner,
        })
        .await?;
    }

    let rendered = render_workflow(SymphonyWorkflowArgs {
        repo_root: args.repo_root.clone(),
        project_slug: args.project_slug.clone(),
        output: args.output.clone(),
        workspace_root: args.workspace_root.clone(),
        base_branch: args.base_branch.clone(),
        max_concurrent_agents: args.max_concurrent_agents,
        poll_interval_ms: args.poll_interval_ms,
        model: args.model.clone(),
        reasoning_effort: args.reasoning_effort.clone(),
        in_progress_state: args.in_progress_state.clone(),
        done_state: args.done_state.clone(),
        blocked_state: args.blocked_state.clone(),
    })
    .await?;

    let symphony_bin = symphony_root.join("bin").join("symphony");
    if !symphony_bin.is_file() {
        bail!(
            "Symphony binary not found at {}; build it first with `cd {} && mix build` or `mise exec -- mix build`",
            symphony_bin.display(),
            symphony_root.display()
        );
    }

    let logs_root = args.logs_root.unwrap_or(rendered.logs_root);
    fs::create_dir_all(&logs_root)
        .with_context(|| format!("failed to create {}", logs_root.display()))?;
    let live_log_path = logs_root.join("log").join("symphony.log");
    println!("workflow: {}", rendered.output_path.display());
    println!("logs root: {}", logs_root.display());
    println!("live log:  {}", live_log_path.display());
    if args.sync_first {
        println!("sync:      completed before launch");
    } else {
        println!("sync:      skipped (use --sync-first to refresh Linear issues first)");
    }

    let mut command = TokioCommand::new(&symphony_bin);
    command
        .current_dir(&symphony_root)
        .arg("--i-understand-that-this-will-be-running-without-the-usual-guardrails")
        .arg("--logs-root")
        .arg(&logs_root)
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit());
    if let Some(port) = args.port {
        command.arg("--port").arg(port.to_string());
    }
    command.arg(&rendered.output_path);

    let mut child = command
        .spawn()
        .with_context(|| format!("failed to launch Symphony from {}", symphony_bin.display()))?;
    let status = child.wait().await.with_context(|| {
        format!(
            "failed waiting for Symphony process from {}",
            symphony_bin.display()
        )
    })?;
    if !status.success() {
        bail!("Symphony exited with status {status}");
    }
    Ok(())
}

fn resolve_symphony_root(explicit_root: Option<PathBuf>) -> Result<PathBuf> {
    if let Some(root) = explicit_root {
        return Ok(root);
    }

    let Some(root) = std::env::var_os(SYMPHONY_ROOT_ENV).filter(|value| !value.is_empty()) else {
        bail!(
            "missing symphony root: pass --symphony-root <path> or set {SYMPHONY_ROOT_ENV}=<path>"
        );
    };

    Ok(PathBuf::from(root))
}

pub(crate) fn resolve_repo_root(repo_root: Option<PathBuf>) -> Result<PathBuf> {
    match repo_root {
        Some(path) => Ok(path),
        None => git_repo_root(),
    }
}

pub(crate) fn resolve_project_slug(repo_root: &Path, cli_slug: Option<&str>) -> Result<String> {
    if let Some(slug) = cli_slug.map(str::trim).filter(|slug| !slug.is_empty()) {
        return Ok(slug.to_string());
    }
    if let Some(slug) = read_existing_workflow_config(repo_root)?.project_slug {
        return Ok(slug);
    }
    bail!(
        "Linear project slug is required for the first Symphony setup; pass --project-slug once or generate .auto/symphony/WORKFLOW.md first"
    );
}

fn resolve_workflow_path(repo_root: &Path, output: Option<PathBuf>) -> PathBuf {
    output.unwrap_or_else(|| repo_root.join(".auto").join("symphony").join("WORKFLOW.md"))
}

fn read_existing_workflow_config(repo_root: &Path) -> Result<WorkflowBootstrapConfig> {
    let workflow_path = resolve_workflow_path(repo_root, None);
    if !workflow_path.is_file() {
        return Ok(WorkflowBootstrapConfig::default());
    }
    let text = fs::read_to_string(&workflow_path)
        .with_context(|| format!("failed to read {}", workflow_path.display()))?;
    let Some(front_matter) = markdown_front_matter(&text) else {
        return Ok(WorkflowBootstrapConfig::default());
    };
    Ok(WorkflowBootstrapConfig {
        project_slug: front_matter_line_value(front_matter, "project_slug"),
    })
}

fn markdown_front_matter(markdown: &str) -> Option<&str> {
    let stripped = markdown.strip_prefix("---\n")?;
    let end = stripped.find("\n---\n")?;
    Some(&stripped[..end])
}

fn front_matter_line_value(front_matter: &str, field: &str) -> Option<String> {
    front_matter.lines().find_map(|line| {
        let trimmed = line.trim_start();
        trimmed
            .strip_prefix(field)
            .and_then(|rest| rest.strip_prefix(':'))
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(unquote_yamlish_scalar)
    })
}

fn unquote_yamlish_scalar(value: &str) -> String {
    value
        .strip_prefix('"')
        .and_then(|trimmed| trimmed.strip_suffix('"'))
        .map(|trimmed| trimmed.replace("\\\"", "\""))
        .or_else(|| {
            value
                .strip_prefix('\'')
                .and_then(|trimmed| trimmed.strip_suffix('\''))
                .map(|trimmed| trimmed.replace("''", "'"))
        })
        .unwrap_or_else(|| value.to_string())
        .trim()
        .to_string()
}

fn resolve_workspace_root(repo_root: &Path, workspace_root: Option<PathBuf>) -> Result<PathBuf> {
    match workspace_root {
        Some(path) => Ok(path),
        None => {
            let base = cache_dir()
                .unwrap_or_else(|| PathBuf::from("/tmp"))
                .join("symphony-workspaces");
            Ok(base.join(repo_name(repo_root)))
        }
    }
}

fn default_logs_root(repo_root: &Path) -> PathBuf {
    repo_root.join(".auto").join("symphony").join("logs")
}

pub(crate) fn resolve_base_branch(
    repo_root: &Path,
    override_branch: Option<String>,
) -> Result<String> {
    if let Some(branch) = override_branch {
        return Ok(branch);
    }
    if let Ok(remote_head) = git_stdout(
        repo_root,
        ["symbolic-ref", "--short", "refs/remotes/origin/HEAD"],
    ) {
        if let Some(branch) = remote_head.trim().strip_prefix("origin/") {
            if !branch.is_empty() {
                return Ok(branch.to_string());
            }
        }
    }
    let current = git_stdout(repo_root, ["branch", "--show-current"])?;
    let current = current.trim();
    if !current.is_empty() {
        return Ok(current.to_string());
    }
    Ok("main".to_string())
}

struct WorkflowRenderSpec<'a> {
    repo_root: &'a Path,
    repo_label: &'a str,
    project_slug: &'a str,
    remote_url: &'a str,
    base_branch: &'a str,
    workspace_root: &'a Path,
    poll_interval_ms: u64,
    max_concurrent_agents: usize,
    model: &'a str,
    reasoning_effort: &'a str,
    todo_state: &'a str,
    in_progress_state: &'a str,
    done_state: &'a str,
    blocked_state: Option<&'a str>,
}

fn render_workflow_markdown(spec: WorkflowRenderSpec<'_>) -> Result<String> {
    validate_workflow_render_spec(&spec)?;
    let shared_cargo_target_dir = shared_cargo_target_dir(spec.workspace_root);
    let workspace_root_text = path_text("workspace root", spec.workspace_root)?;
    let repo_root_text = path_text("repo root", spec.repo_root)?;
    let shared_cargo_target_dir_text =
        path_text("shared Cargo target dir", &shared_cargo_target_dir)?;
    let workspace_root_yaml = yaml_double_quote(&workspace_root_text);
    let shared_cargo_target_dir_yaml = yaml_double_quote(&shared_cargo_target_dir_text);
    let base_branch_shell = shell_quote(spec.base_branch);
    let origin_base_branch_shell = shell_quote(&format!("origin/{}", spec.base_branch));
    let origin_base_range_shell = shell_quote(&format!("origin/{}..HEAD", spec.base_branch));
    let model_reasoning_effort = ["model_reasoning_effort=", spec.reasoning_effort].concat();
    let model_reasoning_effort_shell = shell_quote(&model_reasoning_effort);
    let model_shell = shell_quote(spec.model);
    let blocked_state_line = spec
        .blocked_state
        .map(|state| format!("- If you hit a true external blocker (missing auth/permissions/secrets), add one precise Linear comment and move the issue to `{state}` before stopping.\n"))
        .unwrap_or_else(|| "- If you hit a true external blocker (missing auth/permissions/secrets), add one precise Linear comment describing the blocker before stopping.\n".to_string());
    let before_run_hook = [
        "set -eu".to_string(),
        format!("mkdir -p {}", shell_quote(&shared_cargo_target_dir_text)),
        "if [ -f .git/info/exclude ]; then".to_string(),
        "  if ! grep -qxF '/.cargo-target' .git/info/exclude; then printf '/.cargo-target\\n' >> .git/info/exclude; fi".to_string(),
        "  if ! grep -qxF '/.cargo-target*' .git/info/exclude; then printf '/.cargo-target*\\n' >> .git/info/exclude; fi".to_string(),
        "fi".to_string(),
        "for stale_cargo_target in .cargo-target .cargo-target-*; do".to_string(),
        "  if [ -e \"$stale_cargo_target\" ] || [ -L \"$stale_cargo_target\" ]; then".to_string(),
        "    echo \"before_run: removing repo-local cargo target path $stale_cargo_target\"".to_string(),
        "    rm -rf \"$stale_cargo_target\"".to_string(),
        "  fi".to_string(),
        "done".to_string(),
        "ln -s ../.cargo-target .cargo-target".to_string(),
        ["git fetch origin ", &base_branch_shell].concat(),
        ["git checkout ", &base_branch_shell].concat(),
        [
            "ahead_commits=$(git rev-list --count ",
            &origin_base_range_shell,
            ")",
        ]
        .concat(),
        "should_rebase=1".to_string(),
        "if [ \"$ahead_commits\" -gt 0 ]; then".to_string(),
        [
            "  merge_base=$(git merge-base HEAD ",
            &origin_base_branch_shell,
            ")",
        ]
        .concat(),
        "  echo \"before_run: found $ahead_commits unpushed local commit(s), restoring them to workspace changes before continuing\"".to_string(),
        "  git reset --mixed \"$merge_base\"".to_string(),
        "  should_rebase=0".to_string(),
        "fi".to_string(),
        "if [ -d .git/rebase-merge ] || [ -d .git/rebase-apply ] || [ -f .git/MERGE_HEAD ] || [ -f .git/CHERRY_PICK_HEAD ]; then".to_string(),
        "  echo \"before_run: unfinished git operation detected, preserving workspace state and skipping rebase sync\"".to_string(),
        "  should_rebase=0".to_string(),
        "fi".to_string(),
        "if git ls-files --unmerged | grep -q .; then".to_string(),
        "  echo \"before_run: unmerged index entries detected, preserving workspace state for repair\"".to_string(),
        "  should_rebase=0".to_string(),
        "fi".to_string(),
        "if ! git diff --quiet || ! git diff --cached --quiet; then".to_string(),
        "  echo \"before_run: dirty worktree, skipping rebase sync to preserve local changes\""
            .to_string(),
        "  should_rebase=0".to_string(),
        "fi".to_string(),
        "if [ \"$should_rebase\" -eq 1 ]; then".to_string(),
        ["  git pull --rebase origin ", &base_branch_shell].concat(),
        "fi".to_string(),
    ]
    .into_iter()
    .map(|line| format!("    {line}"))
    .collect::<Vec<_>>()
    .join("\n");
    let codex_command = [
        "env CARGO_TARGET_DIR=",
        &shell_quote(&shared_cargo_target_dir_text),
        " auto quota open codex --config shell_environment_policy.inherit=all --config ",
        &model_reasoning_effort_shell,
        " --model ",
        &model_shell,
        " app-server",
    ]
    .concat();
    Ok(format!(
        "---\n\
tracker:\n  kind: linear\n  api_key: $LINEAR_API_KEY\n  project_slug: {project_slug_yaml}\n  active_states:\n    - {todo_state_yaml}\n    - {in_progress_state_yaml}\n  terminal_states:\n    - Closed\n    - Cancelled\n    - Canceled\n    - Duplicate\n    - {done_state_yaml}\n\
polling:\n  interval_ms: {poll_interval_ms}\n\
workspace:\n  root: {workspace_root_yaml}\n\
hooks:\n  after_create: |\n    git clone --depth 1 {remote_url} .\n  before_run: |\n{before_run_hook}\n  timeout_ms: 300000\n\
agent:\n  max_concurrent_agents: {max_concurrent_agents}\n  max_turns: 20\n\
codex:\n  command: >-\n    {codex_command}\n  approval_policy: never\n  thread_sandbox: workspace-write\n  turn_sandbox_policy:\n    type: workspaceWrite\n    writableRoots:\n      - {workspace_root_yaml}\n      - {shared_cargo_target_dir_yaml}\n  read_timeout_ms: 60000\n  max_turn_wall_clock_ms: 1800000\n  max_turn_total_tokens: 12000000\n---\n\n\
You are running an unattended implementation-plan execution session for repository `{repo_label}`.\n\n\
Repository root inside the workspace clone: `{repo_root_text}`\n\
Integration branch: `{base_branch}`\n\
Linear project: `{project_slug}`\n\n\
{{% if attempt %}}\n\
Continuation context:\n\n\
- This is retry attempt #{{{{ attempt }}}} because the issue remained active.\n\
- Resume from the current workspace state instead of restarting from scratch.\n\
- Do not repeat already-finished investigation or validation unless your code changes require it.\n\
{{% if resume_reason %}}- Failure context from the previous attempt: {{{{ resume_reason }}}}\n\
{{% endif %}}{{% if resume_guidance %}}- Recovery guidance: {{{{ resume_guidance }}}}\n\
{{% endif %}}{{% endif %}}\n\n\
Issue context:\n\
Identifier: {{{{ issue.identifier }}}}\n\
Title: {{{{ issue.title }}}}\n\
Current status: {{{{ issue.state }}}}\n\
URL: {{{{ issue.url }}}}\n\n\
Description:\n\
{{% if issue.description %}}\n\
{{{{ issue.description }}}}\n\
{{% else %}}\n\
No description provided.\n\
{{% endif %}}\n\n\
You must execute the task body from the issue description as the source of truth. The description came from `IMPLEMENTATION_PLAN.md` and includes the task id, acceptance criteria, verification commands, and scope boundary.\n\n\
Operating rules:\n\n\
- Read and follow the repository's `AGENTS.md` plus any directly referenced repo docs before editing code.\n\
- Work only inside the provided repository clone.\n\
- Use targeted validation only; do not widen scope with broad workspace tests.\n\
- Before making changes, search the codebase, tests, and planning artifacts. Do not assume a surface is missing until you verify it.\n\
- Build a short task brief for yourself before editing: task id, spec refs, owned surfaces, integration touchpoints, scope boundary, acceptance criteria, verification, and any assumptions you are relying on.\n\
- Restate the task's assumptions and success conditions from repo evidence before editing. If the task contract is ambiguous, resolve the ambiguity from repo evidence or leave a precise blocker instead of guessing.\n\
- Keep changes scoped to the issue's task body. Do not silently take on unrelated cleanup.\n\
- One issue = one task = one landing attempt. Never mark more than one plan task done, never append `REVIEW.md` handoff text for a second task, and never treat adjacent cleanup as free work.\n\
- Do not mark adjacent tasks done just because the current diff incidentally helps them. Leave those tasks untouched for their own issue unless the plan contract explicitly says this issue owns them.\n\
- Never ask a human to perform follow-up work during normal execution.\n\
{blocked_state_line}\
- Before editing, fetch the current issue via `linear_graphql`, inspect the team states, and if the issue is in `{todo_state}`, move it to `{in_progress_state}`.\n\
- Work directly on `{base_branch}` in this clone. Fresh workspaces are synced from `origin/{base_branch}` before the first turn.\n\
- If you are resuming a dirty workspace after a retry or stall, preserve that local state instead of trying to rebase it before continuing.\n\
- Never run `git fetch`, `git pull`, `git rebase`, `git push`, or branch-switching commands yourself in this workspace. Use `git status`, `git diff`, `git log`, and `git show` for inspection only; Symphony performs sync and landing host-side.\n\
- Do not run the final `git add` or `git commit` flow yourself; Symphony performs landing host-side.\n\
- Never request interactive user input or MCP elicitation. This is a non-interactive unattended run, so make the narrowest reasonable assumption from the issue, repo, and current workspace instead.\n\
- Do not keep multiple long-running shell sessions alive at once. Finish or abandon one long-running `exec_command` session before starting another.\n\
- For `cargo test`, `cargo check`, `cargo build`, `xtask`, and other compile-heavy commands, set the initial `yield_time_ms` high enough to cover the expected runtime instead of polling every few seconds or every minute.\n\
- Do not babysit background compiles with repeated `write_stdin` polls when a single longer wait would do. Prefer one generous wait over many short polls.\n\
- Do not start a second Cargo compile/test/check command while another Cargo command is still running in the same lane unless the issue explicitly requires it.\n\
- If the workspace contains conflict markers, unmerged files, or other repair debt from a prior attempt, fix that workspace integrity problem first before resuming feature work.\n\
- If `apply_patch` verification fails repeatedly, stop repeating the same patch shape. Re-read the file on disk and switch to smaller exact-context edits or a targeted full-file rewrite.\n\
- Before changing task or issue completion state, run a targeted grep or equivalent acceptance check against each acceptance criterion so shipping status cannot outrun actual delivery.\n\
- Never rewrite `IMPLEMENTATION_PLAN.md` prose. The only allowed plan edit is changing the matching task line from `- [ ]` or `- [!]` to `- [x]` when that task is actually complete. Do not edit repo-level rules, acceptance criteria, verification blocks, dependencies, scope boundaries, or unrelated task statuses.\n\
- If you touch `IMPLEMENTATION_PLAN.md`, run `scripts/check-plan-integrity.sh` before landing and fix any reported drift.\n\
- Use the inherited shared `CARGO_TARGET_DIR` from Symphony for Cargo commands. Do not override it with workspace-local or ad hoc temp paths, and do not create `/.cargo-target/` inside the repo clone. If that directory appears, delete it before landing.\n\
- If repo docs mention a fresh isolated Cargo target dir for local development, that guidance is overridden in Symphony sessions. Never prefix Cargo with a different `CARGO_TARGET_DIR`, never invent `/.cargo-target*` variants such as `/.cargo-target-rso29/`, and if `/.cargo-target` is present in the repo clone it must remain the shared `../.cargo-target` symlink.\n\
- If the repo contains `scripts/run-task-verification.sh`, run the concrete executable verification commands through that wrapper instead of invoking them bare. Do not treat narrative `Verification:` prose as literal shell input; if the task only gives prose, derive the narrowest truthful executable proof yourself and record blockers honestly instead of patching the wrapper.\n\
- Never hand-edit verification receipt files. They are execution evidence, not notes.\n\
- If the repo contains `scripts/check-task-scope.py`, run `python3 scripts/check-task-scope.py --staged` before landing. If adjacent integration edits outside the owned or touchpoint surfaces are genuinely required, keep them minimal and record them under `Scope exceptions:` in the task's `REVIEW.md` handoff with a one-line reason per path.\n\
- A task is only ready for `- [x]` or a terminal issue state when local review handoff, verification evidence, and declared completion artifacts are all present. If any of that evidence is still missing, leave the task as `- [~]` or unfinished instead of bluffing it done.\n\
- When the task is complete, mark the matching task in `IMPLEMENTATION_PLAN.md` as `- [x]` instead of deleting it so downstream dependency truth remains visible.\n\
- Append a `REVIEW.md` handoff entry before landing. Preserve the existing file style when present; if `REVIEW.md` is missing, create it with a simple awaiting-review section. Include the task id, changed files or surfaces, `Scope exceptions: none` or the explicit exception list, the exact validation commands you actually ran, and any remaining blockers or `none`.\n\
- When the task is complete, run the verification required by the issue description, then call `symphony_land_issue` with `{{\"baseBranch\":\"{base_branch}\",\"doneState\":\"{done_state}\"}}`. That host-side tool commits the implementation plus the `IMPLEMENTATION_PLAN.md` and `REVIEW.md` artifact updates, rebases onto `origin/{base_branch}`, pushes, and only then moves the issue to `{done_state}`.\n\
- If `symphony_land_issue` reports a rebase conflict, stop retrying the same land immediately. Inspect the conflicting files against `origin/{base_branch}`, integrate the latest base-branch changes into your workspace, rerun targeted validation, and only then try landing again.\n\
- Before starting another exploration turn, inspect the current diff and outstanding acceptance criteria. If the same blocker persists across two consecutive turns or a turn ends without new diff or verification progress, stop looping, leave one precise Linear comment, and move the issue to blocked if such a state exists.\n\
- If validation fails, fix the issue instead of leaving partial work behind.\n\
- Final response should contain only: changed files, validation run, and any remaining blockers.\n\n\
Use these exact GraphQL operations when you need to inspect states or update the issue state:\n\n\
```graphql\n\
query IssueContext($id: String!) {{\n\
  issue(id: $id) {{\n\
    id\n\
    identifier\n\
    state {{\n\
      name\n\
    }}\n\
    team {{\n\
      states(first: 50) {{\n\
        nodes {{\n\
          id\n\
          name\n\
          type\n\
        }}\n\
      }}\n\
    }}\n\
  }}\n\
}}\n\
```\n\n\
```graphql\n\
mutation UpdateIssueState($id: String!, $stateId: String!) {{\n\
  issueUpdate(id: $id, input: {{stateId: $stateId}}) {{\n\
    success\n\
  }}\n\
}}\n\
```\n\n\
```graphql\n\
mutation AddComment($issueId: String!, $body: String!) {{\n\
  commentCreate(input: {{issueId: $issueId, body: $body}}) {{\n\
    success\n\
  }}\n\
}}\n\
```\n",
        project_slug = spec.project_slug,
        project_slug_yaml = yaml_double_quote(spec.project_slug),
        todo_state = spec.todo_state,
        todo_state_yaml = yaml_double_quote(spec.todo_state),
        in_progress_state = spec.in_progress_state,
        in_progress_state_yaml = yaml_double_quote(spec.in_progress_state),
        done_state = spec.done_state,
        done_state_yaml = yaml_double_quote(spec.done_state),
        poll_interval_ms = spec.poll_interval_ms,
        remote_url = shell_quote(spec.remote_url),
        base_branch = spec.base_branch,
        before_run_hook = before_run_hook,
        max_concurrent_agents = spec.max_concurrent_agents,
        codex_command = codex_command,
        repo_label = spec.repo_label,
        repo_root_text = repo_root_text,
        workspace_root_yaml = workspace_root_yaml,
        shared_cargo_target_dir_yaml = shared_cargo_target_dir_yaml,
        blocked_state_line = blocked_state_line,
    ))
}

fn shared_cargo_target_dir(workspace_root: &Path) -> PathBuf {
    workspace_root.join(".cargo-target")
}

fn validate_workflow_render_spec(spec: &WorkflowRenderSpec<'_>) -> Result<()> {
    validate_single_line_scalar("repo label", spec.repo_label)?;
    validate_single_line_scalar("project slug", spec.project_slug)?;
    validate_single_line_scalar("remote URL", spec.remote_url)?;
    validate_branch_name(spec.base_branch)?;
    validate_token_scalar("model", spec.model)?;
    validate_token_scalar("reasoning effort", spec.reasoning_effort)?;
    validate_single_line_scalar("todo state", spec.todo_state)?;
    validate_single_line_scalar("in-progress state", spec.in_progress_state)?;
    validate_single_line_scalar("done state", spec.done_state)?;
    if let Some(blocked_state) = spec.blocked_state {
        validate_single_line_scalar("blocked state", blocked_state)?;
    }
    path_text("repo root", spec.repo_root)?;
    path_text("workspace root", spec.workspace_root)?;
    Ok(())
}

fn validate_branch_name(branch: &str) -> Result<()> {
    validate_single_line_scalar("base branch", branch)?;
    if branch.starts_with('-') || !branch.chars().all(is_safe_branch_char) {
        bail!("invalid base branch `{branch}`; use only letters, digits, '.', '-', '_', or '/'");
    }
    if branch.starts_with('/')
        || branch.ends_with('/')
        || branch.contains("..")
        || branch.contains("//")
        || branch.contains("@{")
        || branch.contains('\\')
    {
        bail!(
            "invalid base branch `{branch}`; use a plain branch name without shell metacharacters or git ref punctuation"
        );
    }
    Ok(())
}

fn validate_token_scalar(label: &str, value: &str) -> Result<()> {
    validate_single_line_scalar(label, value)?;
    if value.starts_with('-') || !value.chars().all(is_safe_token_char) {
        bail!(
            "invalid {label} `{value}`; use only letters, digits, '.', '-', '_', '/', ':', or '+'"
        );
    }
    Ok(())
}

fn validate_single_line_scalar(label: &str, value: &str) -> Result<()> {
    if value.trim().is_empty() {
        bail!("invalid {label}; value must not be empty");
    }
    if value
        .chars()
        .any(|ch| ch == '\n' || ch == '\r' || ch.is_control())
    {
        bail!("invalid {label}; value must be a single line without control characters");
    }
    Ok(())
}

fn path_text(label: &str, path: &Path) -> Result<String> {
    let text = path.display().to_string();
    validate_single_line_scalar(label, &text)?;
    Ok(text)
}

fn is_safe_token_char(ch: char) -> bool {
    ch.is_ascii_alphanumeric() || matches!(ch, '.' | '-' | '_' | '/' | ':' | '+')
}

fn is_safe_branch_char(ch: char) -> bool {
    ch.is_ascii_alphanumeric() || matches!(ch, '.' | '-' | '_' | '/')
}

fn yaml_double_quote(raw: &str) -> String {
    format!("\"{}\"", raw.replace('\\', "\\\\").replace('"', "\\\""))
}

fn shell_quote(raw: &str) -> String {
    format!("'{}'", raw.replace('\'', "'\"'\"'"))
}

#[cfg(test)]
mod tests {
    use super::{
        markdown_front_matter, render_workflow_markdown, resolve_symphony_root, shell_quote,
        WorkflowRenderSpec, SYMPHONY_ROOT_ENV,
    };
    use std::ffi::OsString;
    use std::path::PathBuf;
    use std::sync::{Mutex, OnceLock};

    struct EnvRestore {
        previous: Option<OsString>,
    }

    impl Drop for EnvRestore {
        fn drop(&mut self) {
            if let Some(previous) = self.previous.take() {
                std::env::set_var(SYMPHONY_ROOT_ENV, previous);
            } else {
                std::env::remove_var(SYMPHONY_ROOT_ENV);
            }
        }
    }

    fn symphony_root_env_lock() -> &'static Mutex<()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
    }

    fn replace_symphony_root_env(value: Option<&str>) -> EnvRestore {
        let previous = std::env::var_os(SYMPHONY_ROOT_ENV);
        if let Some(value) = value {
            std::env::set_var(SYMPHONY_ROOT_ENV, value);
        } else {
            std::env::remove_var(SYMPHONY_ROOT_ENV);
        }
        EnvRestore { previous }
    }

    #[test]
    fn run_requires_symphony_root_when_unset() {
        let _guard = symphony_root_env_lock().lock().expect("env lock");
        let _restore = replace_symphony_root_env(None);

        let error = resolve_symphony_root(None).expect_err("missing root should fail");
        let message = error.to_string();

        assert!(message.contains("missing symphony root"));
        assert!(message.contains("--symphony-root <path>"));
        assert!(message.contains("AUTODEV_SYMPHONY_ROOT=<path>"));
    }

    #[test]
    fn run_uses_symphony_root_env_when_arg_missing() {
        let _guard = symphony_root_env_lock().lock().expect("env lock");
        let _restore = replace_symphony_root_env(Some("/tmp/autodev-symphony"));

        let root = resolve_symphony_root(None).expect("env root should resolve");

        assert_eq!(root, PathBuf::from("/tmp/autodev-symphony"));
    }

    #[test]
    fn run_symphony_root_arg_overrides_env() {
        let _guard = symphony_root_env_lock().lock().expect("env lock");
        let _restore = replace_symphony_root_env(Some("/tmp/autodev-env-symphony"));

        let root = resolve_symphony_root(Some(PathBuf::from("/tmp/autodev-cli-symphony")))
            .expect("explicit root should resolve");

        assert_eq!(root, PathBuf::from("/tmp/autodev-cli-symphony"));
    }

    #[test]
    fn workflow_render_is_repo_specific() {
        let repo_root = PathBuf::from("/home/r/Coding/autonomy");
        let workspace_root = PathBuf::from("/tmp/symphony-workspaces/autonomy");
        let markdown = render_workflow_markdown(test_workflow_spec(
            repo_root.as_path(),
            workspace_root.as_path(),
            "git@github.com:example/autonomy.git",
            "trunk",
            "gpt-5.5",
            "high",
        ))
        .expect("workflow should render");
        assert!(markdown.contains("project_slug: \"autonomy-symphony\""));
        assert!(markdown.contains("git clone --depth 1 'git@github.com:example/autonomy.git' ."));
        assert!(markdown.contains("mkdir -p '/tmp/symphony-workspaces/autonomy/.cargo-target'"));
        assert!(markdown.contains("printf '/.cargo-target\\n' >> .git/info/exclude"));
        assert!(markdown.contains("printf '/.cargo-target*\\n' >> .git/info/exclude"));
        assert!(markdown.contains("removing repo-local cargo target path $stale_cargo_target"));
        assert!(markdown.contains("ln -s ../.cargo-target .cargo-target"));
        assert!(markdown.contains("git fetch origin 'trunk'"));
        assert!(markdown.contains("git rev-list --count 'origin/trunk..HEAD'"));
        assert!(markdown.contains("should_rebase=1"));
        assert!(markdown.contains("git reset --mixed \"$merge_base\""));
        assert!(markdown.contains("unfinished git operation detected"));
        assert!(markdown.contains("unmerged index entries detected"));
        assert!(markdown.contains("if ! git diff --quiet || ! git diff --cached --quiet; then"));
        assert!(markdown.contains("restoring them to workspace changes before continuing"));
        assert!(markdown.contains("skipping rebase sync to preserve local changes"));
        assert!(markdown.contains("root: \"/tmp/symphony-workspaces/autonomy\""));
        assert!(markdown.contains("Failure context from the previous attempt"));
        assert!(markdown.contains("Recovery guidance"));
        assert!(markdown.contains("mark the matching task in `IMPLEMENTATION_PLAN.md` as `- [x]`"));
        assert!(markdown
            .contains("Fresh workspaces are synced from `origin/trunk` before the first turn."));
        assert!(markdown.contains("If you are resuming a dirty workspace after a retry or stall"));
        assert!(markdown.contains("Never run `git fetch`, `git pull`, `git rebase`, `git push`, or branch-switching commands yourself"));
        assert!(markdown.contains("Do not run the final `git add` or `git commit` flow yourself"));
        assert!(markdown.contains("Never request interactive user input or MCP elicitation"));
        assert!(markdown.contains("Do not keep multiple long-running shell sessions alive at once"));
        assert!(markdown
            .contains("Do not babysit background compiles with repeated `write_stdin` polls"));
        assert!(markdown.contains("Do not start a second Cargo compile/test/check command"));
        assert!(markdown.contains("Build a short task brief for yourself before editing"));
        assert!(markdown.contains("One issue = one task = one landing attempt"));
        assert!(markdown.contains("Do not mark adjacent tasks done"));
        assert!(markdown.contains("If `apply_patch` verification fails repeatedly"));
        assert!(markdown.contains("Never rewrite `IMPLEMENTATION_PLAN.md` prose"));
        assert!(markdown.contains("run `scripts/check-plan-integrity.sh` before landing"));
        assert!(markdown.contains("Use the inherited shared `CARGO_TARGET_DIR` from Symphony"));
        assert!(markdown.contains("do not create `/.cargo-target/` inside the repo clone"));
        assert!(markdown.contains("If repo docs mention a fresh isolated Cargo target dir"));
        assert!(markdown.contains("never invent `/.cargo-target*` variants"));
        assert!(markdown.contains("If the repo contains `scripts/run-task-verification.sh`"));
        assert!(markdown.contains("Never hand-edit verification receipt files"));
        assert!(markdown.contains("If the repo contains `scripts/check-task-scope.py`"));
        assert!(markdown.contains("Scope exceptions: none"));
        assert!(markdown.contains("Append a `REVIEW.md` handoff entry before landing."));
        assert!(markdown.contains("If the same blocker persists across two consecutive turns"));
        assert!(markdown.contains("max_turn_wall_clock_ms: 1800000"));
        assert!(markdown.contains("max_turn_total_tokens: 12000000"));
        assert!(markdown.contains("read_timeout_ms: 60000"));
        assert!(markdown.contains("command: >-"));
        assert!(markdown.contains("turn_sandbox_policy:"));
        assert!(markdown.contains("writableRoots:"));
        assert!(markdown.contains("      - \"/tmp/symphony-workspaces/autonomy\""));
        assert!(markdown.contains("      - \"/tmp/symphony-workspaces/autonomy/.cargo-target\""));
        assert!(markdown.contains("env CARGO_TARGET_DIR="));
        assert!(markdown.contains("'/tmp/symphony-workspaces/autonomy/.cargo-target'"));
        assert!(markdown.contains(
            "call `symphony_land_issue` with `{\"baseBranch\":\"trunk\",\"doneState\":\"Done\"}`"
        ));
        assert!(markdown.contains("If `symphony_land_issue` reports a rebase conflict"));
    }

    #[test]
    fn workflow_render_rejects_hostile_branch() {
        let repo_root = PathBuf::from("/home/r/Coding/autonomy");
        let workspace_root = PathBuf::from("/tmp/symphony-workspaces/autonomy");
        let error = render_workflow_markdown(test_workflow_spec(
            repo_root.as_path(),
            workspace_root.as_path(),
            "git@github.com:example/autonomy.git",
            "main; touch /tmp/pwned",
            "gpt-5.5",
            "high",
        ))
        .expect_err("hostile branch should be rejected");

        assert!(error.to_string().contains("base branch"));
    }

    #[test]
    fn workflow_render_rejects_hostile_model_and_effort() {
        let repo_root = PathBuf::from("/home/r/Coding/autonomy");
        let workspace_root = PathBuf::from("/tmp/symphony-workspaces/autonomy");

        let model_error = render_workflow_markdown(test_workflow_spec(
            repo_root.as_path(),
            workspace_root.as_path(),
            "git@github.com:example/autonomy.git",
            "trunk",
            "gpt-5.5 --dangerously-bypass-approvals-and-sandbox",
            "high",
        ))
        .expect_err("hostile model should be rejected");
        assert!(model_error.to_string().contains("model"));

        let effort_error = render_workflow_markdown(test_workflow_spec(
            repo_root.as_path(),
            workspace_root.as_path(),
            "git@github.com:example/autonomy.git",
            "trunk",
            "gpt-5.5",
            "high\nwritableRoots:",
        ))
        .expect_err("hostile effort should be rejected");
        assert!(effort_error.to_string().contains("reasoning effort"));

        let remote_error = render_workflow_markdown(test_workflow_spec(
            repo_root.as_path(),
            workspace_root.as_path(),
            "git@github.com:example/autonomy.git\n  timeout_ms: 1",
            "trunk",
            "gpt-5.5",
            "high",
        ))
        .expect_err("hostile remote URL should be rejected");
        assert!(remote_error.to_string().contains("remote URL"));

        let hostile_workspace_root = PathBuf::from("/tmp/symphony\nhooks:");
        let path_error = render_workflow_markdown(test_workflow_spec(
            repo_root.as_path(),
            hostile_workspace_root.as_path(),
            "git@github.com:example/autonomy.git",
            "trunk",
            "gpt-5.5",
            "high",
        ))
        .expect_err("hostile path should be rejected");
        assert!(path_error.to_string().contains("workspace root"));

        let quoted_workspace_root = PathBuf::from("/tmp/symphony workspaces/auto'quote");
        let markdown = render_workflow_markdown(test_workflow_spec(
            repo_root.as_path(),
            quoted_workspace_root.as_path(),
            "git@github.com:example/autonomy.git",
            "trunk",
            "gpt-5.5",
            "high",
        ))
        .expect("paths with spaces and quotes should render safely");
        assert!(
            markdown.contains("mkdir -p '/tmp/symphony workspaces/auto'\"'\"'quote/.cargo-target'")
        );
        assert!(markdown.contains("root: \"/tmp/symphony workspaces/auto'quote\""));
    }

    fn test_workflow_spec<'a>(
        repo_root: &'a std::path::Path,
        workspace_root: &'a std::path::Path,
        remote_url: &'a str,
        base_branch: &'a str,
        model: &'a str,
        reasoning_effort: &'a str,
    ) -> WorkflowRenderSpec<'a> {
        WorkflowRenderSpec {
            repo_root,
            repo_label: "autonomy",
            project_slug: "autonomy-symphony",
            remote_url,
            base_branch,
            workspace_root,
            poll_interval_ms: 5000,
            max_concurrent_agents: 1,
            model,
            reasoning_effort,
            todo_state: "Todo",
            in_progress_state: "In Progress",
            done_state: "Done",
            blocked_state: Some("Backlog"),
        }
    }

    #[test]
    fn markdown_front_matter_extracts_project_slug() {
        let workflow = r#"---
tracker:
  kind: linear
  project_slug: "autonomy-symphony"
---

body
"#;
        let front_matter = markdown_front_matter(workflow).expect("front matter");
        assert!(front_matter.contains("project_slug: \"autonomy-symphony\""));
    }

    #[test]
    fn shell_quote_escapes_single_quotes() {
        assert_eq!(shell_quote("a'b"), "'a'\"'\"'b'");
    }
}
