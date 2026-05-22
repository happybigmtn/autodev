pub(crate) const AUTODEV_PROMPT_ETHOS_MARKER: &str = "## Autodev Builder Ethos";
pub(crate) const PRODUCTION_ORCHESTRATION_DISCIPLINE: &str = r#"Production orchestration discipline:
- Treat a partially completed repo with an active `IMPLEMENTATION_PLAN.md`, `WORKLIST.md`, `plans/`, or equivalent queue as a live production playground. Reconcile and sharpen that queue instead of creating a competing plan universe.
- Rank work by direct production leverage: first unblock compile/runtime/source-of-truth contracts, then executable user/operator workflows, then reusable UI/runtime component contracts, then tests that guard those contracts, then docs/evidence only when they directly unblock those slices.
- Choose the next autodev lever explicitly: recommend `auto design` when a reusable UI/runtime contract or component system is missing, `auto gen` when doctrine is sound but specs/queue need regeneration, `auto parallel` when the root queue is implementation-ready, and continuing or supervising an existing run when it is already landing the top blockers. State restart/conflict risk before recommending a new run.
- Evidence budget: do not create standalone audit, receipt, checkpoint, or evidence tasks unless later implementation depends on a named unresolved decision. Every evidence task must name the exact implementation it unlocks and the decision that cannot be made without it. Delete, demote, or mark not-doing stale evidence-only items that no longer steer the next executable slice.
- Never leave the next executor with only docs-only, report-only, artifact-only, or low-priority cleanup while high-count runtime, user-facing, or source-of-truth blockers remain.
- For design work and `auto design`, prefer shared runtime-backed components, interaction contracts, and reusable visual systems over one-off screen, route, game, fixture, or screenshot polish. A design task is queue-ready only when it names the runtime/API owner, UI consumers, generated artifacts or `none`, fixture boundary, and executable proof."#;

const AUTODEV_PROMPT_ETHOS: &str = r#"## Autodev Builder Ethos

These principles apply to every autodev model-backed phase:

1. Close the highest-leverage complete slice. Completeness means the smallest user-visible or operator-visible improvement that is fully implemented, tested, and evidenced. Do not maximize artifact volume. Prefer one priority decision, one bounded implementation slice, and one narrow proof over broad audits, reports, or speculative backlog expansion.
2. Search before building. Inspect the live repo, existing helpers, generated clients, runtime owners, and ecosystem-standard patterns before inventing new machinery. The best answer often reuses what already exists, then adds one first-principles insight the repo was missing.
3. User sovereignty. The operator decides. When a recommendation changes the user's stated direction, present the tradeoff and the missing context instead of silently overruling them.
4. Runtime truth before presentation. Engine/API/runtime code owns canonical facts. UI and docs render those facts through existing helpers or generated contracts. Do not create fake mockups, manual bindings, fixture fallbacks, or duplicated business logic as if they were product truth.
5. Evidence or it did not happen. Every plan, implementation, review, audit, and ship claim needs narrow proof that would fail if the original problem returned.

Source inspiration: gstack ETHOS.md (completeness, Search Before Building, User Sovereignty). Apply it as working doctrine, not as permission to ignore repo instructions.
"#;

pub(crate) fn with_autodev_prompt_ethos(prompt: &str) -> String {
    if prompt.contains(AUTODEV_PROMPT_ETHOS_MARKER) {
        return prompt.to_string();
    }
    format!("{AUTODEV_PROMPT_ETHOS}\n\n{prompt}")
}

#[cfg(test)]
mod tests {
    use super::{with_autodev_prompt_ethos, AUTODEV_PROMPT_ETHOS_MARKER};

    #[test]
    fn ethos_is_prepended_once() {
        let prompt = with_autodev_prompt_ethos("Do work.");
        assert!(prompt.starts_with(AUTODEV_PROMPT_ETHOS_MARKER));
        assert!(prompt.contains("highest-leverage complete slice"));
        assert!(prompt.contains("Runtime truth before presentation"));
        assert!(super::PRODUCTION_ORCHESTRATION_DISCIPLINE.contains("live production playground"));
        assert!(super::PRODUCTION_ORCHESTRATION_DISCIPLINE
            .contains("Choose the next autodev lever explicitly"));
        assert!(super::PRODUCTION_ORCHESTRATION_DISCIPLINE.contains("`auto design`"));

        let second = with_autodev_prompt_ethos(&prompt);
        assert_eq!(second, prompt);
    }
}
