//! Shared composition primitive for autodev prompts.
//!
//! Before this module existed there were ~52 `fn build_*_prompt` functions across
//! `parallel_command.rs`, `audit_everything.rs`, `super_command.rs`, `bug_command.rs`,
//! `nemesis.rs`, and others. They independently re-implemented the same surface
//! shape: a doctrine preamble, a role line, an edit-boundary clause, an
//! input/output contract, an evidence checklist, and a verdict footer. A change
//! in one place did not propagate.
//!
//! `PromptSpec` is the data model; `render()` produces the final string. Builders
//! migrate one at a time -- this module does not force a rewrite of the entire
//! repo on day one.

use crate::prompt_ethos::{with_autodev_prompt_ethos, with_lane_doctrine, LANE_DOCTRINE_MARKER};
use crate::verdict::verdict_footer;

/// One slot in the input/output contract of a prompt.
#[derive(Clone, Debug)]
pub(crate) struct PromptSlot {
    pub(crate) label: String,
    pub(crate) body: String,
}

impl PromptSlot {
    pub(crate) fn new(label: impl Into<String>, body: impl Into<String>) -> Self {
        Self {
            label: label.into(),
            body: body.into(),
        }
    }
}

/// Whether the rendered prompt should carry the lane doctrine block (the seven
/// invariants that apply to anything that touches runtime/UI). Suppress for
/// pure review or summarization prompts where the doctrine is irrelevant.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum EthosPosture {
    /// Prepend ethos + lane doctrine (the default for code-writing prompts).
    Full,
    /// Prepend ethos only (review/summary prompts that do not edit code).
    EthosOnly,
    /// No preamble (tests, machine-readable shims).
    None,
}

#[derive(Clone, Debug)]
pub(crate) struct PromptSpec {
    pub(crate) role: String,
    pub(crate) edit_boundary: Option<String>,
    pub(crate) inputs: Vec<PromptSlot>,
    pub(crate) outputs: Vec<PromptSlot>,
    pub(crate) evidence: Vec<String>,
    pub(crate) verdict_alternatives: Option<Vec<String>>,
    pub(crate) ethos: EthosPosture,
    pub(crate) freeform_tail: Option<String>,
}

impl PromptSpec {
    pub(crate) fn new(role: impl Into<String>) -> Self {
        Self {
            role: role.into(),
            edit_boundary: None,
            inputs: Vec::new(),
            outputs: Vec::new(),
            evidence: Vec::new(),
            verdict_alternatives: None,
            ethos: EthosPosture::Full,
            freeform_tail: None,
        }
    }

    pub(crate) fn ethos(mut self, ethos: EthosPosture) -> Self {
        self.ethos = ethos;
        self
    }

    pub(crate) fn edit_boundary(mut self, body: impl Into<String>) -> Self {
        self.edit_boundary = Some(body.into());
        self
    }

    pub(crate) fn input(mut self, label: impl Into<String>, body: impl Into<String>) -> Self {
        self.inputs.push(PromptSlot::new(label, body));
        self
    }

    pub(crate) fn output(mut self, label: impl Into<String>, body: impl Into<String>) -> Self {
        self.outputs.push(PromptSlot::new(label, body));
        self
    }

    pub(crate) fn evidence_item(mut self, item: impl Into<String>) -> Self {
        self.evidence.push(item.into());
        self
    }

    pub(crate) fn verdicts<I, S>(mut self, allowed: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.verdict_alternatives = Some(allowed.into_iter().map(Into::into).collect());
        self
    }

    pub(crate) fn freeform_tail(mut self, body: impl Into<String>) -> Self {
        self.freeform_tail = Some(body.into());
        self
    }

    pub(crate) fn render(&self) -> String {
        let mut sections: Vec<String> = Vec::with_capacity(8);

        sections.push(format!("# Role\n\n{}", self.role.trim()));

        if let Some(boundary) = &self.edit_boundary {
            sections.push(format!("# Edit boundary\n\n{}", boundary.trim()));
        }

        if !self.inputs.is_empty() {
            let mut block = String::from("# Inputs\n");
            for slot in &self.inputs {
                block.push_str(&format!("\n## {}\n\n{}\n", slot.label, slot.body.trim()));
            }
            sections.push(block);
        }

        if !self.outputs.is_empty() {
            let mut block = String::from("# Outputs\n");
            for slot in &self.outputs {
                block.push_str(&format!("\n## {}\n\n{}\n", slot.label, slot.body.trim()));
            }
            sections.push(block);
        }

        if !self.evidence.is_empty() {
            let mut block = String::from("# Evidence checklist\n");
            for item in &self.evidence {
                block.push_str(&format!("\n- {item}"));
            }
            sections.push(block);
        }

        if let Some(allowed) = &self.verdict_alternatives {
            let refs: Vec<&str> = allowed.iter().map(String::as_str).collect();
            sections.push(verdict_footer(&refs));
        }

        if let Some(tail) = &self.freeform_tail {
            sections.push(tail.trim().to_string());
        }

        let body = sections.join("\n\n");

        match self.ethos {
            EthosPosture::None => body,
            EthosPosture::EthosOnly => with_autodev_prompt_ethos(&body),
            EthosPosture::Full => {
                // Doctrine first (closer to the role), then ethos wraps everything.
                let with_doctrine = if body.contains(LANE_DOCTRINE_MARKER) {
                    body
                } else {
                    with_lane_doctrine(&body)
                };
                with_autodev_prompt_ethos(&with_doctrine)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{EthosPosture, PromptSpec};
    use crate::prompt_ethos::{AUTODEV_PROMPT_ETHOS_MARKER, LANE_DOCTRINE_MARKER};

    #[test]
    fn default_render_includes_ethos_and_doctrine() {
        let rendered = PromptSpec::new("Implement task T-1.").render();
        assert!(rendered.contains(AUTODEV_PROMPT_ETHOS_MARKER));
        assert!(rendered.contains(LANE_DOCTRINE_MARKER));
        assert!(rendered.contains("# Role"));
    }

    #[test]
    fn ethos_only_omits_doctrine() {
        let rendered = PromptSpec::new("Review the diff.")
            .ethos(EthosPosture::EthosOnly)
            .render();
        assert!(rendered.contains(AUTODEV_PROMPT_ETHOS_MARKER));
        assert!(!rendered.contains(LANE_DOCTRINE_MARKER));
    }

    #[test]
    fn none_posture_skips_preamble() {
        let rendered = PromptSpec::new("Schema validator")
            .ethos(EthosPosture::None)
            .render();
        assert!(!rendered.contains(AUTODEV_PROMPT_ETHOS_MARKER));
        assert!(!rendered.contains(LANE_DOCTRINE_MARKER));
    }

    #[test]
    fn sections_are_emitted_in_canonical_order() {
        let rendered = PromptSpec::new("Author the report.")
            .edit_boundary("Write to OUTPUT.md only.")
            .input("Prior analysis", "## Findings\n- F-1\n")
            .output("Report", "Single markdown file")
            .evidence_item("Cite each finding by ID")
            .verdicts(["Verdict: GO", "Verdict: NO-GO"])
            .ethos(EthosPosture::None)
            .render();

        let role_idx = rendered.find("# Role").unwrap();
        let edit_idx = rendered.find("# Edit boundary").unwrap();
        let in_idx = rendered.find("# Inputs").unwrap();
        let out_idx = rendered.find("# Outputs").unwrap();
        let ev_idx = rendered.find("# Evidence checklist").unwrap();
        let verdict_idx = rendered.find("Verdict:").unwrap();

        assert!(role_idx < edit_idx);
        assert!(edit_idx < in_idx);
        assert!(in_idx < out_idx);
        assert!(out_idx < ev_idx);
        assert!(ev_idx < verdict_idx);
    }

    #[test]
    fn verdict_footer_lists_allowed_alternatives() {
        let rendered = PromptSpec::new("Review")
            .verdicts(["Verdict: PASS", "Verdict: NO-GO"])
            .ethos(EthosPosture::None)
            .render();
        assert!(rendered.contains("Verdict: PASS"));
        assert!(rendered.contains("Verdict: NO-GO"));
    }
}
