pub(crate) const AUTODEV_PROMPT_ETHOS_MARKER: &str = "## Autodev Builder Ethos";

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

/// Stage doctrine: the seven invariants every lane/audit/super worker honors.
///
/// This block historically lived inline in `parallel_command.rs`, `audit_everything.rs`,
/// and `super_command.rs`. Keeping it here as a single const means doctrine evolves
/// in one place and prompts stay cache-friendly across calls.
pub(crate) const LANE_DOCTRINE_BLOCK: &str = "- Source-of-truth discipline: runtime/engine/API owners define facts; UI/presentation code renders those facts. Do not duplicate runtime-owned catalogs, constants, settlement math, risk classifications, eligibility rules, balances, or status derivations in UI code.
- Runtime-first order: when the task touches both runtime and UI, implement or confirm the runtime/API contract first, regenerate/check generated bindings or schemas second, then update UI consumers.
- Fixture boundary: production code must not import fixture/demo/sample data as fallback truth. Fixture data belongs in tests, stories, demos, or explicit dev-only harnesses.
- Contract generation: if the task names generated artifacts or changes runtime/API shapes, run the named generator/check or record `AUTO_ENV_BLOCKER`/`AUTO_VERIFICATION_BLOCKER` with the exact reason it could not run.
- Cross-surface proof: if UI consumers are named, include at least one runtime-output-to-UI/readback proof or a clear blocker. Component-only tests are insufficient when the original risk is runtime/UI drift.
- Retire-first cleanup: if the task names retired or superseded surfaces, delete/archive/tombstone them and clean callers/indexes in the same lane when in scope. Do not leave stale active doctrine as a TODO unless the task explicitly gates it.
- Independent closeout: before your final answer, re-check the original task fields (`Source of truth`, `Runtime owner`, `UI consumers`, `Generated artifacts`, `Fixture boundary`, `Retired surfaces`, and `Review/closeout`) and state how each was satisfied or blocked.";

pub(crate) const LANE_DOCTRINE_MARKER: &str = "Source-of-truth discipline:";

/// Inject the lane doctrine block once. Idempotent: re-application is a no-op so
/// callers can freely compose without checking.
pub(crate) fn with_lane_doctrine(prompt: &str) -> String {
    if prompt.contains(LANE_DOCTRINE_MARKER) {
        return prompt.to_string();
    }
    format!("{prompt}\n\n{LANE_DOCTRINE_BLOCK}\n")
}

#[cfg(test)]
mod tests {
    use super::{
        with_autodev_prompt_ethos, with_lane_doctrine, AUTODEV_PROMPT_ETHOS_MARKER,
        LANE_DOCTRINE_BLOCK, LANE_DOCTRINE_MARKER,
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
    fn lane_doctrine_contains_all_seven_invariants() {
        for invariant in [
            "Source-of-truth discipline",
            "Runtime-first order",
            "Fixture boundary",
            "Contract generation",
            "Cross-surface proof",
            "Retire-first cleanup",
            "Independent closeout",
        ] {
            assert!(
                LANE_DOCTRINE_BLOCK.contains(invariant),
                "doctrine missing invariant `{invariant}`"
            );
        }
    }

    #[test]
    fn lane_doctrine_is_idempotent() {
        let prompt = with_lane_doctrine("Worker brief.");
        assert!(prompt.contains(LANE_DOCTRINE_MARKER));
        let second = with_lane_doctrine(&prompt);
        assert_eq!(second, prompt);
    }
}
