use std::path::PathBuf;

use crate::loop_command::queue::LoopQueueSnapshot;

pub(crate) const DEFAULT_LOOP_PROMPT_TEMPLATE: &str = r#"0a. Study `AGENTS.md` for repo-specific build, validation, and staging rules.
0b. Study `IMPLEMENTATION_PLAN.md` and identify the first actionable unfinished task marked `- [ ]` or `- [~]` whose explicit dependencies are already satisfied. Treat tasks marked `- [!]` as blocked and skip them unless they are later unblocked. If a `- [~]` row explicitly names `Completion path: <TASK-ID>`, treat it as a historical gap record and implement the named completion-path task instead of replaying the old row.
0c. Study `specs/*` with full repo context, but when multiple dated specs cover the same surface, treat the newest spec referenced by the current unchecked task as authoritative. Older or duplicate specs are historical context only.
0d. Use the specs, plan, and the live codebase as a single contract. If they disagree, treat the code and the current task's authoritative specs as evidence, record the conflict truthfully, and do not bluff your way through it.
0e. For every current-state fact, trust the live codebase over planning artifacts unless the code is plainly stale and the repo includes stronger primary-source evidence.
0f. When additional repositories are listed below, inspect and edit them directly when the current task's owned surfaces, acceptance criteria, or blocker evidence point there. Read each touched repo's own `AGENTS.md` and operational docs before editing it.

1. Your task is to implement functionality per the specifications using the full repository context.
   - Follow `IMPLEMENTATION_PLAN.md` in order and take the next actionable unfinished `- [ ]` or `- [~]` task from top to bottom.
   - Do not reprioritize the queue yourself.
   - Do not stop on earlier `- [!]` tasks; they are blocked and not runnable in this iteration.
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
   - When you finish a task, preserve its row in `IMPLEMENTATION_PLAN.md` and mark it `- [x]` only when local verification, review handoff, and required completion artifacts are actually in place.
   - If code lands but the local evidence is still incomplete, mark the task `- [~]` instead of bluffing it to done.
   - When a task is blocked by an external dependency or owner decision, mark it as `- [!]` and record the blocker under that task.
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
   - Convert the task marker from `- [ ]` to `- [!]` and record the blocker under the task in `IMPLEMENTATION_PLAN.md`.
   - Commit the planning update if it materially changes the execution record.
   - Move to the next actionable unfinished `- [ ]` or `- [~]` task instead of repeating the same failed attempt.

7. Task-order rule:
   - Treat the order in `IMPLEMENTATION_PLAN.md` as authoritative.
   - Work on the first actionable unfinished `- [ ]` or `- [~]` task unless its explicit dependencies are still unchecked.
   - Treat `- [!]` tasks as blocked and skip them while selecting work.
   - If the current task is already satisfied, mark it `- [x]`, append a truthful note to `COMPLETED.md`, and continue downward.

8. Branch rule:
   - Work only on branch `{branch}`.
   - Do not create or push feature branches, lane branches, or topic branches.

99999. Important: keep `AGENTS.md` operational only.
999999. Important: prefer complete working increments over placeholders.
9999999. Important: if unrelated tests fail and they prevent a truthful green result, fix them as part of the increment.
99999999. CRITICAL: Do not assume functionality is missing — search the codebase to confirm before implementing anything new.
999999999. Every new module must be importable and wired into the package. Dead code that isn't reachable from any entry point is an island — wire it before committing.
9999999999. When you learn something new about how to build, run, or validate the repo, update `AGENTS.md` — but keep it brief and operational only.
99999999999. A task is not done because the code looks right. It is done when the acceptance criteria are satisfied and the verification evidence is real.
999999999999. Shell safety: never pass file contents or large strings (>50KB) as inline shell command arguments — write them to a temp file instead. Narrow glob patterns with directory prefixes so they cannot expand to thousands of paths and hit the OS argument limit.
9999999999999. Search resilience: treat empty Grep/Glob/Find results as evidence, not proof a surface is missing. If an exact symbol search misses, inspect the containing enum/struct/module, nearby tests, and the latest compiler/test errors before retrying the search.
99999999999999. Search futility: if the same search tool returns empty results 3 times in a row, stop and re-evaluate your approach. The thing you are looking for may not exist, may be named differently, or may live in a different location. Prefer behavior-level searches and current code definitions over stale symbol names."#;

pub(crate) fn render_default_loop_prompt(branch: &str, reference_repos: &[PathBuf]) -> String {
    append_reference_repo_clause(
        DEFAULT_LOOP_PROMPT_TEMPLATE.replace("{branch}", branch),
        reference_repos,
    )
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
        "{prompt}\n\nAdditional repositories you may inspect or edit when the task contract points there:\n{listing}\n\nRepository-crossing rules:\n- If the current task's owned surfaces live in one of these repos, implement the code change there instead of pretending the queue repo should own it.\n- Keep `IMPLEMENTATION_PLAN.md` truthful as the active queue for this run even when code lands in another repo.\n- Read each touched repo's `AGENTS.md`, tests, and operational docs before editing it.\n- Commit and push each touched repo separately.\n"
    )
}

pub(crate) fn build_iteration_prompt(prompt_template: &str, queue: &LoopQueueSnapshot) -> String {
    let blocked_clause = if queue.blocked_ids.is_empty() {
        "Blocked tasks marked `- [!]`: none".to_string()
    } else {
        format!(
            "Blocked tasks marked `- [!]` to skip this iteration: {}",
            queue.blocked_ids.join(", ")
        )
    };
    format!(
        "{prompt_template}\n\nCurrent queue state for this iteration:\n- First actionable unfinished task: `{}`\n- Unfinished task count: {}\n- {}\n\nExecute the instructions above.",
        queue.pending_ids[0],
        queue.pending_ids.len(),
        blocked_clause
    )
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::{build_iteration_prompt, render_default_loop_prompt};
    use crate::loop_command::queue::LoopQueueSnapshot;

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
        assert!(prompt.contains("Treat tasks marked `- [!]` as blocked"));
        assert!(prompt.contains("next actionable unfinished `- [ ]` or `- [~]` task"));
        assert!(prompt.contains("Completion path: <TASK-ID>"));
    }

    #[test]
    fn default_prompt_lists_reference_repos_when_declared() {
        let prompt = render_default_loop_prompt("main", &[PathBuf::from("/tmp/robopokermulti")]);
        assert!(prompt.contains("Additional repositories you may inspect or edit"));
        assert!(prompt.contains("/tmp/robopokermulti"));
        assert!(prompt.contains("owned surfaces live in one of these repos"));
    }

    #[test]
    fn iteration_prompt_injects_actionable_and_blocked_tasks() {
        let queue = LoopQueueSnapshot {
            pending_ids: vec!["META-001".to_string(), "GATE-P4".to_string()],
            blocked_ids: vec!["DEC-001".to_string()],
        };
        let prompt = build_iteration_prompt("base prompt", &queue);

        assert!(prompt.contains("First actionable unfinished task: `META-001`"));
        assert!(prompt.contains("Unfinished task count: 2"));
        assert!(prompt.contains("Blocked tasks marked `- [!]` to skip this iteration: DEC-001"));
    }
}
