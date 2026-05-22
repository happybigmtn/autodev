//! Codex-backed sync planner: prompt building, invocation, and response normalization.

use std::collections::{HashMap, HashSet, VecDeque};
use std::fs;
use std::path::Path;
use std::process::Stdio;

use anyhow::{anyhow, bail, Context, Result};
use serde::Deserialize;
use serde_json::Value;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt};
use tokio::process::Command as TokioCommand;

use crate::codex_stream::capture_codex_output_with_heartbeat;
use crate::prompt_ethos::with_autodev_prompt_ethos;
use crate::quota_config::Provider;
use crate::quota_exec;
use crate::symphony_command::task::{render_sync_task_digest, SymphonyTask};
use crate::util::{atomic_write, repo_name};

pub(crate) const SYNC_PLANNER_MAX_PRIORITY: i64 = 4;

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct EffectiveTaskSchedule {
    pub(crate) dependencies: Vec<String>,
    external_dependencies: Vec<String>,
    pub(crate) priority: i64,
    rationale: String,
}

#[derive(Debug, Deserialize)]
struct PlannerResponse {
    #[serde(default)]
    strategy_summary: String,
    tasks: Vec<PlannerTask>,
}

#[derive(Debug, Deserialize)]
struct PlannerTask {
    task_id: String,
    priority: i64,
    #[allow(dead_code)]
    #[serde(default)]
    dependencies: Vec<String>,
    #[serde(default)]
    external_dependencies: Vec<String>,
    #[serde(default)]
    rationale: String,
}

#[derive(Clone, Debug, Default)]
pub(crate) struct DeterminedSyncPlan {
    pub(crate) strategy_summary: String,
    pub(crate) task_plans: HashMap<String, EffectiveTaskSchedule>,
}

impl DeterminedSyncPlan {
    pub(crate) fn fallback(tasks: &[SymphonyTask]) -> Self {
        let priorities = fallback_task_priorities(tasks);
        let mut task_plans = HashMap::new();
        for task in tasks {
            let priority = priorities
                .get(&task.id)
                .copied()
                .unwrap_or(SYNC_PLANNER_MAX_PRIORITY);
            task_plans.insert(
                task.id.clone(),
                EffectiveTaskSchedule {
                    dependencies: dedup_task_refs(task.dependencies.clone()),
                    external_dependencies: Vec::new(),
                    priority,
                    rationale: "deterministic fallback from explicit Dependencies lines"
                        .to_string(),
                },
            );
        }
        Self {
            strategy_summary: "deterministic fallback from explicit Dependencies lines".to_string(),
            task_plans,
        }
    }
}

pub(crate) async fn determine_sync_plan(
    repo_root: &Path,
    plan_text: &str,
    tasks: &[SymphonyTask],
    codex_bin: &Path,
    model: &str,
    reasoning_effort: &str,
) -> Result<DeterminedSyncPlan> {
    let planner_dir = repo_root.join(".auto").join("symphony");
    fs::create_dir_all(&planner_dir)
        .with_context(|| format!("failed to create {}", planner_dir.display()))?;
    let prompt = build_sync_planner_prompt(repo_root, plan_text, tasks);
    let prompt_path = planner_dir.join("sync-planner-prompt.md");
    let raw_response_path = planner_dir.join("sync-planner-response.jsonl");
    let stderr_path = planner_dir.join("sync-planner-stderr.log");
    let parsed_response_path = planner_dir.join("sync-planner-result.json");
    atomic_write(&prompt_path, prompt.as_bytes())
        .with_context(|| format!("failed to write {}", prompt_path.display()))?;
    println!(
        "planner: analyzing {} pending task(s) in `{}` with {} / {}",
        tasks.len(),
        repo_name(repo_root),
        model,
        reasoning_effort
    );
    println!("planner prompt: {}", prompt_path.display());
    println!("planner raw output: {}", raw_response_path.display());
    println!("planner stderr: {}", stderr_path.display());

    let (stdout_raw, stderr_text) =
        run_codex_planner(repo_root, &prompt, model, reasoning_effort, codex_bin).await?;
    atomic_write(&raw_response_path, stdout_raw.as_bytes())
        .with_context(|| format!("failed to write {}", raw_response_path.display()))?;
    atomic_write(&stderr_path, stderr_text.as_bytes())
        .with_context(|| format!("failed to write {}", stderr_path.display()))?;

    let planner_message = extract_agent_message_from_codex_stream(&stdout_raw)
        .ok_or_else(|| anyhow!("Codex planner did not emit a final agent_message"))?;
    let planner_json = extract_planner_json(&planner_message)
        .ok_or_else(|| anyhow!("Codex planner response did not contain valid JSON"))?;
    atomic_write(&parsed_response_path, planner_json.as_bytes())
        .with_context(|| format!("failed to write {}", parsed_response_path.display()))?;
    let parsed: PlannerResponse = serde_json::from_str(&planner_json)
        .with_context(|| "failed to parse Codex planner JSON response")?;
    normalize_planner_response(tasks, parsed)
}

fn build_sync_planner_prompt(repo_root: &Path, plan_text: &str, tasks: &[SymphonyTask]) -> String {
    let task_ids = tasks
        .iter()
        .map(|task| format!("`{}`", task.id))
        .collect::<Vec<_>>()
        .join(", ");
    let preamble = plan_preamble(plan_text);
    let task_digests = tasks
        .iter()
        .map(render_sync_task_digest)
        .collect::<Vec<_>>()
        .join("\n\n");
    format!(
        r#"You are planning issue dispatch for `auto symphony sync`.

Repository: `{repo}`
Repo root: `{repo_root}`
Goal: produce a dependency DAG and Linear priorities that maximize safe throughput for 5 concurrent Symphony lanes.

This is a concrete planning deliverable, not a quick heuristic pass. Treat it like a Codex work item:
- inspect the live repository when needed
- use tools to verify queue facts
- leave scratch notes or drafts under `.auto/symphony/` if that helps you reason clearly before the final JSON

Constraints:
- `IMPLEMENTATION_PLAN.md` is the primary source of truth, but you may inspect the live repo to resolve ambiguous shared surfaces or blocker language.
- Preserve every explicit prerequisite from the plan.
- Treat each task's `Dependencies:` block as the authoritative machine blocker set for repo-local scheduling.
- Do not invent new repo-local `dependencies` from critical-path prose, parenthetical notes, "parallel with" commentary, merge-conflict caution, or broad shared-surface anxiety. Use `priority` and `rationale` to shape waves instead.
- If prose gating looks real but is not encoded in the task contract, reflect it in `priority`/`rationale` rather than smuggling in a hidden blocker. That kind of fix belongs in the plan itself.
- Be conservative about merge-conflict risk, but do not serialize unrelated work unnecessarily.
- Use `priority` values `1` through `4`, where `1` is the first work Symphony should prefer.
- Treat `priority: 1` as the immediate first-wave launch set for a 5-lane run, not a broad bucket for every early task.
- Prefer roughly 3-7 tasks at `priority: 1`. If more tasks are technically runnable, push the less urgent ones to `priority: 2` or add blockers so the top wave stays intentional.
- Use `priority: 2` for the immediate next wave after the first launch set, `priority: 3` for post-foundation or expansion-gated work, and `priority: 4` for late, conditional, or externally blocked work.
- When two tasks are both early but one is clearly more central to shared foundations, MVP gating, or unblock sequencing, do not leave them tied at `priority: 1` just because both are runnable.
- `dependencies` must list task IDs already present in that task's explicit dependency contract after normalizing obvious narrative wrappers.
- Put cross-repo or otherwise unsynced blockers in `external_dependencies`.
- Return every pending task exactly once. Do not omit any task and do not invent new task IDs.
- Before finalizing, do at least one concrete verification pass with tools so the run stays observable and grounded.
- If the queue is large, create a compact scratch summary such as `.auto/symphony/sync-planner-working.md` or `.auto/symphony/sync-planner-working.json` before you emit the final answer.
- Before finalizing, check the size of the `priority: 1` set and tighten it if it is too broad for a 5-lane start.
- Respond with JSON only. No prose outside the JSON object. No code fences.

Pending task IDs:
{task_ids}

Return this exact schema:
{{
  "strategy_summary": "short explanation",
  "tasks": [
    {{
      "task_id": "P-000",
      "priority": 1,
      "dependencies": ["P-001"],
      "external_dependencies": ["OTHER-123"],
      "rationale": "short scheduling reason"
    }}
  ]
}}

Queue preamble:

```md
{preamble}
```

Pending task digests:

```md
{task_digests}
```
"#,
        repo = repo_name(repo_root),
        repo_root = repo_root.display(),
        task_ids = task_ids,
        preamble = preamble,
        task_digests = task_digests
    )
}

async fn run_codex_planner(
    repo_root: &Path,
    prompt: &str,
    model: &str,
    reasoning_effort: &str,
    codex_bin: &Path,
) -> Result<(String, String)> {
    let prompt = with_autodev_prompt_ethos(prompt);
    let mut command = planner_command(repo_root, model, reasoning_effort, codex_bin)?;
    command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .current_dir(repo_root);

    let mut child = command
        .spawn()
        .with_context(|| format!("failed to launch sync planner from {}", repo_root.display()))?;

    let mut stdin = child
        .stdin
        .take()
        .context("Codex planner stdin should be piped")?;
    stdin
        .write_all(prompt.as_bytes())
        .await
        .context("failed to write sync planner prompt to Codex")?;
    drop(stdin);

    let stdout = child
        .stdout
        .take()
        .context("Codex planner stdout should be piped")?;
    let stderr = child
        .stderr
        .take()
        .context("Codex planner stderr should be piped")?;

    let stdout_task = tokio::spawn(async move {
        capture_codex_output_with_heartbeat(stdout, "sync planner", 15).await
    });
    let stderr_task = tokio::spawn(async move { read_stream(stderr).await });

    let status = child
        .wait()
        .await
        .context("failed waiting for Codex planner")?;
    let stdout_raw = stdout_task
        .await
        .context("Codex planner stdout capture task panicked")??;
    let stderr_text = stderr_task
        .await
        .context("Codex planner stderr capture task panicked")??;

    if !status.success() {
        bail!(
            "Codex planner failed: {}",
            if stderr_text.trim().is_empty() {
                stdout_raw.trim()
            } else {
                stderr_text.trim()
            }
        );
    }
    Ok((stdout_raw, stderr_text))
}

fn planner_command(
    repo_root: &Path,
    model: &str,
    reasoning_effort: &str,
    codex_bin: &Path,
) -> Result<TokioCommand> {
    let mut command = if quota_exec::is_quota_available(Provider::Codex) {
        let auto_bin = std::env::current_exe().context("failed to resolve current auto binary")?;
        let mut command = TokioCommand::new(auto_bin);
        command.arg("quota").arg("open").arg("codex").arg("exec");
        command
    } else {
        TokioCommand::new(codex_bin)
    };
    command
        .arg("--json")
        .arg("--dangerously-bypass-approvals-and-sandbox")
        .arg("--skip-git-repo-check")
        .arg("--cd")
        .arg(repo_root)
        .arg("-m")
        .arg(model)
        .arg("-c")
        .arg(format!("model_reasoning_effort=\"{reasoning_effort}\""));
    Ok(command)
}

fn normalize_planner_response(
    tasks: &[SymphonyTask],
    response: PlannerResponse,
) -> Result<DeterminedSyncPlan> {
    let known_ids = tasks
        .iter()
        .map(|task| task.id.clone())
        .collect::<HashSet<_>>();
    let mut by_id = HashMap::<String, PlannerTask>::new();
    for task in response.tasks {
        if !known_ids.contains(&task.task_id) {
            bail!("Codex planner returned unknown task `{}`", task.task_id);
        }
        if by_id.insert(task.task_id.clone(), task).is_some() {
            bail!("Codex planner returned duplicate task entry");
        }
    }
    for task in tasks {
        if !by_id.contains_key(&task.id) {
            bail!("Codex planner omitted task `{}`", task.id);
        }
    }

    let mut task_plans = HashMap::new();
    for task in tasks {
        let planned = by_id
            .remove(&task.id)
            .with_context(|| format!("Codex planner omitted task `{}`", task.id))?;
        let mut dependencies = task.dependencies.clone();
        dependencies.retain(|dependency| dependency != &task.id);
        dependencies = dedup_task_refs(dependencies);

        let mut external_dependencies = planned.external_dependencies;
        external_dependencies.extend(
            dependencies
                .iter()
                .filter(|dependency| !known_ids.contains((*dependency).as_str()))
                .cloned(),
        );
        external_dependencies = dedup_task_refs(external_dependencies);

        task_plans.insert(
            task.id.clone(),
            EffectiveTaskSchedule {
                dependencies,
                external_dependencies,
                priority: planned.priority.clamp(1, SYNC_PLANNER_MAX_PRIORITY),
                rationale: planned.rationale.trim().to_string(),
            },
        );
    }

    validate_schedule_dag(tasks, &task_plans)?;

    Ok(DeterminedSyncPlan {
        strategy_summary: response.strategy_summary.trim().to_string(),
        task_plans,
    })
}

fn validate_schedule_dag(
    tasks: &[SymphonyTask],
    task_plans: &HashMap<String, EffectiveTaskSchedule>,
) -> Result<()> {
    let task_ids = tasks
        .iter()
        .map(|task| task.id.clone())
        .collect::<HashSet<_>>();
    let mut indegree = HashMap::<String, usize>::new();
    let mut dependents = HashMap::<String, Vec<String>>::new();
    for task in tasks {
        let internal_deps = task_plans
            .get(&task.id)
            .map(|schedule| {
                schedule
                    .dependencies
                    .iter()
                    .filter(|dependency| task_ids.contains((*dependency).as_str()))
                    .cloned()
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        indegree.insert(task.id.clone(), internal_deps.len());
        for dependency in internal_deps {
            dependents
                .entry(dependency)
                .or_default()
                .push(task.id.clone());
        }
    }

    let order = task_order_map(tasks);
    let mut queue = tasks
        .iter()
        .filter(|task| indegree.get(&task.id).copied().unwrap_or(0) == 0)
        .map(|task| task.id.clone())
        .collect::<VecDeque<_>>();
    let mut visited = 0usize;

    while let Some(task_id) = queue.pop_front() {
        visited += 1;
        let mut children = dependents.remove(&task_id).unwrap_or_default();
        children.sort_by_key(|task| order.get(task).copied().unwrap_or(usize::MAX));
        for child in children {
            let entry = indegree
                .get_mut(&child)
                .with_context(|| format!("missing indegree for task `{child}`"))?;
            *entry = entry.saturating_sub(1);
            if *entry == 0 {
                queue.push_back(child);
            }
        }
    }

    if visited != tasks.len() {
        bail!("planner dependency graph contains a cycle");
    }
    Ok(())
}

fn fallback_task_priorities(tasks: &[SymphonyTask]) -> HashMap<String, i64> {
    let order = task_order_map(tasks);
    let task_ids = tasks
        .iter()
        .map(|task| task.id.clone())
        .collect::<HashSet<_>>();
    let mut indegree = HashMap::<String, usize>::new();
    let mut dependents = HashMap::<String, Vec<String>>::new();
    let mut max_parent_wave = HashMap::<String, usize>::new();
    let mut waves = HashMap::<String, usize>::new();

    for task in tasks {
        let internal_deps = task
            .dependencies
            .iter()
            .filter(|dependency| task_ids.contains((*dependency).as_str()))
            .cloned()
            .collect::<Vec<_>>();
        indegree.insert(task.id.clone(), internal_deps.len());
        for dependency in internal_deps {
            dependents
                .entry(dependency)
                .or_default()
                .push(task.id.clone());
        }
    }

    let mut queue = tasks
        .iter()
        .filter(|task| indegree.get(&task.id).copied().unwrap_or(0) == 0)
        .map(|task| task.id.clone())
        .collect::<VecDeque<_>>();

    while let Some(task_id) = queue.pop_front() {
        let current_wave = max_parent_wave.get(&task_id).copied().unwrap_or(0);
        waves.insert(task_id.clone(), current_wave);
        let mut children = dependents.remove(&task_id).unwrap_or_default();
        children.sort_by_key(|task| order.get(task).copied().unwrap_or(usize::MAX));
        for child in children {
            let child_wave = max_parent_wave.entry(child.clone()).or_insert(0);
            *child_wave = (*child_wave).max(current_wave + 1);
            let entry = indegree.get_mut(&child).expect("child indegree must exist");
            *entry = entry.saturating_sub(1);
            if *entry == 0 {
                queue.push_back(child);
            }
        }
    }

    let mut fallback_wave = waves.values().copied().max().unwrap_or(0) + 1;
    for task in tasks {
        if waves.contains_key(&task.id) {
            continue;
        }
        waves.insert(task.id.clone(), fallback_wave);
        fallback_wave += 1;
    }

    tasks
        .iter()
        .map(|task| {
            let wave = waves.get(&task.id).copied().unwrap_or(3);
            (
                task.id.clone(),
                (wave as i64 + 1).clamp(1, SYNC_PLANNER_MAX_PRIORITY),
            )
        })
        .collect()
}

fn task_order_map(tasks: &[SymphonyTask]) -> HashMap<String, usize> {
    tasks
        .iter()
        .enumerate()
        .map(|(index, task)| (task.id.clone(), index))
        .collect()
}

fn dedup_task_refs(refs: Vec<String>) -> Vec<String> {
    let mut seen = HashSet::new();
    let mut deduped = Vec::new();
    for reference in refs {
        let normalized = reference.trim();
        if normalized.is_empty() || !seen.insert(normalized.to_string()) {
            continue;
        }
        deduped.push(normalized.to_string());
    }
    deduped
}

fn plan_preamble(plan_text: &str) -> String {
    let mut lines = Vec::new();
    for line in plan_text.lines() {
        let trimmed = line.trim_start();
        if trimmed.starts_with("- [ ] ")
            || trimmed.starts_with("- [!] ")
            || trimmed.starts_with("- [x] ")
            || trimmed.starts_with("- [X] ")
        {
            break;
        }
        lines.push(line.to_string());
    }
    lines.join("\n")
}

fn extract_agent_message_from_codex_stream(raw: &str) -> Option<String> {
    let mut last_message = None;
    for line in raw.lines() {
        let Ok(value) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        if let Some(message) = value
            .get("item")
            .and_then(|item| item.get("type"))
            .and_then(Value::as_str)
            .filter(|item_type| *item_type == "agent_message")
            .and_then(|_| value.get("item"))
            .and_then(|item| item.get("text"))
            .and_then(Value::as_str)
        {
            last_message = Some(message.to_string());
            continue;
        }
        if let Some(message) = value.get("last_agent_message").and_then(Value::as_str) {
            last_message = Some(message.to_string());
        }
    }
    last_message
}

fn extract_planner_json(content: &str) -> Option<String> {
    let trimmed = content.trim();
    if trimmed.is_empty() {
        return None;
    }
    if serde_json::from_str::<Value>(trimmed).is_ok() {
        return Some(trimmed.to_string());
    }
    if let Some(fenced) = extract_fenced_json_block(trimmed) {
        if serde_json::from_str::<Value>(&fenced).is_ok() {
            return Some(fenced);
        }
    }
    let prefix = extract_complete_json_value_prefix(trimmed)?;
    serde_json::from_str::<Value>(&prefix).ok()?;
    Some(prefix)
}

fn extract_complete_json_value_prefix(content: &str) -> Option<String> {
    let content = content.trim_start();
    let mut stream = serde_json::Deserializer::from_str(content).into_iter::<Value>();
    stream.next()?.ok()?;
    let end = stream.byte_offset();
    Some(content[..end].trim_end().to_string())
}

fn extract_fenced_json_block(content: &str) -> Option<String> {
    let trimmed = content.trim();
    if !trimmed.starts_with("```") {
        return None;
    }
    let mut lines = trimmed.lines();
    let opening = lines.next()?.trim();
    if !opening.starts_with("```") {
        return None;
    }

    let mut body = Vec::new();
    for line in lines {
        if line.trim_start().starts_with("```") {
            return Some(body.join("\n").trim().to_string());
        }
        body.push(line.to_string());
    }
    None
}

async fn read_stream<R>(stream: R) -> Result<String>
where
    R: AsyncRead + Unpin,
{
    let mut reader = tokio::io::BufReader::new(stream);
    let mut text = String::new();
    reader
        .read_to_string(&mut text)
        .await
        .context("failed to read stream")?;
    Ok(text)
}

#[cfg(test)]
mod tests {
    use crate::symphony_command::task::{SymphonyTask, TaskStatus};

    use super::{
        extract_agent_message_from_codex_stream, fallback_task_priorities,
        normalize_planner_response, PlannerResponse, PlannerTask,
    };

    #[test]
    fn fallback_priorities_follow_dependency_waves() {
        let tasks = vec![
            SymphonyTask {
                id: "P-001".to_string(),
                title: "foundation".to_string(),
                status: TaskStatus::Pending,
                dependencies: Vec::new(),
                markdown: String::new(),
            },
            SymphonyTask {
                id: "P-002".to_string(),
                title: "depends on foundation".to_string(),
                status: TaskStatus::Pending,
                dependencies: vec!["P-001".to_string()],
                markdown: String::new(),
            },
            SymphonyTask {
                id: "P-003".to_string(),
                title: "deep dependency".to_string(),
                status: TaskStatus::Pending,
                dependencies: vec!["P-002".to_string()],
                markdown: String::new(),
            },
        ];
        let priorities = fallback_task_priorities(&tasks);
        assert_eq!(priorities.get("P-001"), Some(&1));
        assert_eq!(priorities.get("P-002"), Some(&2));
        assert_eq!(priorities.get("P-003"), Some(&3));
    }

    #[test]
    fn normalize_planner_response_keeps_explicit_machine_dependencies() {
        let tasks = vec![
            SymphonyTask {
                id: "P-001".to_string(),
                title: "foundation".to_string(),
                status: TaskStatus::Pending,
                dependencies: Vec::new(),
                markdown: String::new(),
            },
            SymphonyTask {
                id: "P-002".to_string(),
                title: "feature".to_string(),
                status: TaskStatus::Pending,
                dependencies: vec!["P-001".to_string()],
                markdown: String::new(),
            },
        ];
        let response = PlannerResponse {
            strategy_summary: "test".to_string(),
            tasks: vec![
                PlannerTask {
                    task_id: "P-001".to_string(),
                    priority: 1,
                    dependencies: Vec::new(),
                    external_dependencies: Vec::new(),
                    rationale: "foundation".to_string(),
                },
                PlannerTask {
                    task_id: "P-002".to_string(),
                    priority: 2,
                    dependencies: vec!["P-003".to_string()],
                    external_dependencies: vec!["EXT-1".to_string()],
                    rationale: "feature".to_string(),
                },
            ],
        };

        let normalized = normalize_planner_response(&tasks, response).expect("planner response");
        assert_eq!(
            normalized.task_plans["P-002"].dependencies,
            vec!["P-001".to_string()]
        );
        assert_eq!(
            normalized.task_plans["P-002"].external_dependencies,
            vec!["EXT-1".to_string()]
        );
    }

    #[test]
    fn codex_agent_message_extraction_skips_banner_lines() {
        let raw = r#"Reading prompt from stdin...
{"type":"thread.started","thread_id":"abc"}
{"type":"item.completed","item":{"id":"item_0","type":"agent_message","text":"{\"ok\":true}"}}
{"type":"turn.completed","usage":{"input_tokens":10,"output_tokens":1}}
"#;
        assert_eq!(
            extract_agent_message_from_codex_stream(raw),
            Some("{\"ok\":true}".to_string())
        );
    }
}
