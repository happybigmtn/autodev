//! Plain data types shared across the `auto bug` pipeline submodules.

use serde::{Deserialize, Serialize};

#[derive(Clone, Debug)]
pub(crate) struct RepoChunk {
    pub(crate) ordinal: usize,
    pub(crate) id: String,
    pub(crate) scope_label: String,
    pub(crate) files: Vec<String>,
    pub(crate) risk_notes: Vec<String>,
}

#[derive(Clone, Debug)]
pub(crate) struct FileCandidate {
    pub(crate) path: String,
    pub(crate) estimated_tokens: usize,
    pub(crate) risk_score: usize,
    pub(crate) risk_notes: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct BugFinding {
    pub(crate) bug_id: String,
    pub(crate) title: String,
    pub(crate) location: String,
    pub(crate) impact: String,
    pub(crate) points: u8,
    pub(crate) description: String,
    pub(crate) why_plausible: String,
    pub(crate) falsification_checks: Vec<String>,
    pub(crate) evidence: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct BugIdRewrite {
    pub(crate) old_id: String,
    pub(crate) new_id: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct SkepticVerdict {
    pub(crate) bug_id: String,
    pub(crate) decision: String,
    pub(crate) confidence_percent: u8,
    pub(crate) counter_argument: String,
    pub(crate) risk_calculation: String,
    pub(crate) follow_up_checks: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct AcceptedFinding {
    pub(crate) bug_id: String,
    pub(crate) chunk_id: String,
    pub(crate) title: String,
    pub(crate) location: String,
    pub(crate) impact: String,
    pub(crate) points: u8,
    pub(crate) description: String,
    pub(crate) why_plausible: String,
    pub(crate) falsification_checks: Vec<String>,
    pub(crate) evidence: Vec<String>,
    pub(crate) skeptic_confidence_percent: u8,
    pub(crate) skeptic_counter_argument: String,
    pub(crate) skeptic_follow_up_checks: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct FixResult {
    pub(crate) bug_id: String,
    pub(crate) status: String,
    pub(crate) summary: String,
    pub(crate) validation_commands: Vec<String>,
    pub(crate) touched_files: Vec<String>,
    pub(crate) residual_risks: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct ReviewResult {
    pub(crate) bug_id: String,
    pub(crate) verdict: String,
    pub(crate) confidence: String,
    pub(crate) notes: String,
    pub(crate) follow_up: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct FinalReviewResult {
    pub(crate) bug_id: String,
    pub(crate) status: String,
    pub(crate) summary: String,
    pub(crate) validation_commands: Vec<String>,
    pub(crate) touched_files: Vec<String>,
    pub(crate) residual_risks: Vec<String>,
}

#[derive(Clone, Debug)]
pub(crate) struct ChunkOutcome {
    pub(crate) chunk: RepoChunk,
    pub(crate) findings: Vec<BugFinding>,
    pub(crate) disproved_count: usize,
    pub(crate) accepted: Vec<AcceptedFinding>,
    pub(crate) verified: Vec<AcceptedFinding>,
    pub(crate) reviews: Vec<ReviewResult>,
}

#[derive(Clone, Debug)]
pub(crate) struct PhaseConfig {
    pub(crate) model: String,
    pub(crate) effort: String,
}
