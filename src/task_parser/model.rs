#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum TaskStatus {
    Pending,
    Partial,
    Blocked,
    Done,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct PlanTask {
    pub(crate) id: String,
    pub(crate) title: String,
    pub(crate) status: TaskStatus,
    pub(crate) markdown: String,
    pub(crate) body: String,
    pub(crate) dependencies: Vec<String>,
    pub(crate) verification_text: Option<String>,
    pub(crate) completion_artifacts: Vec<String>,
    pub(crate) completion_path_target: Option<String>,
    pub(crate) lane_kind: Option<LaneKind>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum LaneKind {
    Code,
    Operator,
    Evidence,
}

impl LaneKind {
    pub(crate) fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "code" => Some(Self::Code),
            "operator" | "operator-action" | "operator action" => Some(Self::Operator),
            "evidence" | "evidence-only" | "proof" | "proof-only" => Some(Self::Evidence),
            _ => None,
        }
    }

    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Code => "code",
            Self::Operator => "operator",
            Self::Evidence => "evidence",
        }
    }
}
