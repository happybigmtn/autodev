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

#[cfg(test)]
mod tests {
    use super::{with_autodev_prompt_ethos, AUTODEV_PROMPT_ETHOS_MARKER};

    #[test]
    fn ethos_is_prepended_once() {
        let prompt = with_autodev_prompt_ethos("Do work.");
        assert!(prompt.starts_with(AUTODEV_PROMPT_ETHOS_MARKER));
        assert!(prompt.contains("Boil the lake"));
        assert!(prompt.contains("Runtime truth before presentation"));

        let second = with_autodev_prompt_ethos(&prompt);
        assert_eq!(second, prompt);
    }
}
