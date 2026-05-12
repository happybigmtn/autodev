pub(crate) const AUTODEV_PROMPT_ETHOS_MARKER: &str = "## Autodev Builder Ethos";
pub(crate) const AUTODEV_GOAL_CONTRACT_MARKER: &str = "## Autodev Goal Contract";

const AUTODEV_PROMPT_ETHOS: &str = r#"## Autodev Builder Ethos

These principles apply to every autodev model-backed phase:

1. Boil the lake. AI-assisted implementation makes completeness cheap. Prefer the complete, tested, observable implementation over the shortcut when the difference is minutes. Do not defer tests, edge cases, error paths, or closeout proof just to save a little work.
2. Search before building. Inspect the live repo, existing helpers, generated clients, runtime owners, and ecosystem-standard patterns before inventing new machinery. The best answer often reuses what already exists, then adds one first-principles insight the repo was missing.
3. User sovereignty. The operator decides. When a recommendation changes the user's stated direction, present the tradeoff and the missing context instead of silently overruling them.
4. Runtime truth before presentation. Engine/API/runtime code owns canonical facts. UI and docs render those facts through existing helpers or generated contracts. Do not create fake mockups, manual bindings, fixture fallbacks, or duplicated business logic as if they were product truth.
5. Evidence or it did not happen. Every plan, implementation, review, audit, and ship claim needs narrow proof that would fail if the original problem returned.

Source inspiration: gstack ETHOS.md (Boil the Lake, Search Before Building, User Sovereignty). Apply it as working doctrine, not as permission to ignore repo instructions.
"#;

pub(crate) fn with_autodev_prompt_ethos(prompt: &str) -> String {
    if prompt.contains(AUTODEV_PROMPT_ETHOS_MARKER) {
        return prompt.to_string();
    }
    format!("{AUTODEV_PROMPT_ETHOS}\n\n{prompt}")
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct GoalContract {
    objective: String,
    success_criteria: Vec<String>,
    non_goals: Vec<String>,
    stop_condition: String,
    evidence: Vec<String>,
}

impl GoalContract {
    pub(crate) fn for_context(context_label: &str) -> Self {
        let context_label = context_label.trim();
        let phase = if context_label.is_empty() {
            "the current autodev phase"
        } else {
            context_label
        };
        Self {
            objective: format!("Complete `{phase}` end to end without losing the operator's intended outcome."),
            success_criteria: vec![
                "Satisfy the assigned prompt, active plan/spec contract, and repo-local instructions.".to_string(),
                "Prefer runtime/source-of-truth changes before presentation changes when both are in scope.".to_string(),
                "Produce the required files, commits, reports, receipts, or queue updates named by the phase.".to_string(),
                "Run the narrowest truthful validation available, and report exact blockers instead of claiming unverified success.".to_string(),
            ],
            non_goals: vec![
                "Do not broaden into unrelated cleanup, refactors, features, or generated artifact churn.".to_string(),
                "Do not replace durable autodev state such as manifests, implementation plans, review handoffs, receipts, or git history with conversational memory.".to_string(),
                "Do not leave human-only review as the stopping condition when an executable proof or autonomous best answer is possible.".to_string(),
            ],
            stop_condition: "Stop only when the phase is complete with durable evidence, or when a precise external blocker is recorded with the next executable recovery step.".to_string(),
            evidence: vec![
                "Changed files or generated artifacts are present at their required paths.".to_string(),
                "Validation commands, receipt files, review reports, or run logs support the completion claim.".to_string(),
                "Mutating implementation lanes finish with a clean worktree and an intentional local commit unless the phase is explicitly report-only.".to_string(),
            ],
        }
    }

    pub(crate) fn render(&self) -> String {
        let success_criteria = render_bullets(&self.success_criteria);
        let non_goals = render_bullets(&self.non_goals);
        let evidence = render_bullets(&self.evidence);
        format!(
            "{AUTODEV_GOAL_CONTRACT_MARKER}\n\nObjective:\n{objective}\n\nSuccess criteria:\n{success_criteria}\nNon-goals:\n{non_goals}\nStop condition:\n{stop_condition}\n\nRequired evidence:\n{evidence}",
            objective = self.objective,
            stop_condition = self.stop_condition,
        )
    }
}

pub(crate) fn with_autodev_prompt_context(prompt: &str, context_label: &str) -> String {
    let prompt = with_autodev_prompt_ethos(prompt);
    if prompt.contains(AUTODEV_GOAL_CONTRACT_MARKER) {
        return prompt;
    }
    let goal = GoalContract::for_context(context_label);
    format!("{}\n\n{}", goal.render(), prompt)
}

fn render_bullets(items: &[String]) -> String {
    if items.is_empty() {
        return "- none\n".to_string();
    }
    items
        .iter()
        .map(|item| format!("- {}\n", item.trim()))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::{
        with_autodev_prompt_context, with_autodev_prompt_ethos, AUTODEV_GOAL_CONTRACT_MARKER,
        AUTODEV_PROMPT_ETHOS_MARKER,
    };

    #[test]
    fn ethos_is_prepended_once() {
        let prompt = with_autodev_prompt_ethos("Do work.");
        assert!(prompt.starts_with(AUTODEV_PROMPT_ETHOS_MARKER));
        assert!(prompt.contains("Boil the lake"));
        assert!(prompt.contains("Runtime truth before presentation"));

        let second = with_autodev_prompt_ethos(&prompt);
        assert_eq!(second, prompt);
    }

    #[test]
    fn goal_contract_wraps_prompt_once() {
        let prompt = with_autodev_prompt_context("Do work.", "auto parallel lane-1 TASK-001");
        assert!(prompt.starts_with(AUTODEV_GOAL_CONTRACT_MARKER));
        assert!(prompt.contains(AUTODEV_PROMPT_ETHOS_MARKER));
        assert!(prompt.contains("Complete `auto parallel lane-1 TASK-001`"));
        assert!(prompt.contains("durable autodev state"));

        let second = with_autodev_prompt_context(&prompt, "ignored");
        assert_eq!(second, prompt);
    }
}
