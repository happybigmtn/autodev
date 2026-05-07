//! Per-lane structured event stream.
//!
//! Lives alongside the existing plaintext `stdout.log` so it never replaces the
//! human-readable lane log. Events are written as one JSON object per line
//! (`events.jsonl`) and consumed by `auto parallel watch`. Format is intentionally
//! minimal: a single timestamp string, an `event_type`, and a free-form details map.
//!
//! Existing host-side log emitters (e.g. [`append_lane_host_event`] in
//! `parallel_command`) call into [`LaneEventLogger::host_message`] so any operator
//! message that already lands in `stdout.log` is also captured here as a
//! structured event. Specific call sites can additionally emit
//! [`LaneEvent::TaskStarted`] / [`LaneEvent::TaskCompleted`] /
//! [`LaneEvent::ReceiptDrift`] / [`LaneEvent::LaneIdle`] when they have richer
//! context to report.

use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

/// File name for the per-lane structured event stream.
pub(crate) const LANE_EVENTS_FILE: &str = "events.jsonl";

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(tag = "event_type", rename_all = "snake_case")]
pub(crate) enum LaneEvent {
    TaskStarted,
    TaskCompleted { outcome: String },
    LaneIdle { summary: String },
    ReceiptDrift { reason: String },
    HostMessage { message: String },
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct LaneEventEnvelope {
    pub(crate) timestamp: String,
    pub(crate) lane_index: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) task_id: Option<String>,
    #[serde(flatten)]
    pub(crate) event: LaneEvent,
}

#[derive(Clone, Debug)]
pub(crate) struct LaneEventLogger {
    events_path: PathBuf,
    lane_index: usize,
}

impl LaneEventLogger {
    pub(crate) fn for_lane(lane_root: &Path, lane_index: usize) -> Self {
        Self {
            events_path: lane_root.join(LANE_EVENTS_FILE),
            lane_index,
        }
    }

    pub(crate) fn emit(&self, task_id: Option<&str>, event: LaneEvent) {
        if let Err(err) = self.try_emit(task_id, event) {
            eprintln!(
                "warning: failed appending lane event to {}: {err:#}",
                self.events_path.display()
            );
        }
    }

    fn try_emit(&self, task_id: Option<&str>, event: LaneEvent) -> Result<()> {
        if let Some(parent) = self.events_path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("failed to create {}", parent.display()))?;
        }
        let envelope = LaneEventEnvelope {
            timestamp: chrono::Utc::now().to_rfc3339(),
            lane_index: self.lane_index,
            task_id: task_id.map(str::to_string),
            event,
        };
        let mut line = serde_json::to_string(&envelope).context("serialize lane event")?;
        line.push('\n');
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.events_path)
            .with_context(|| format!("failed to open {}", self.events_path.display()))?;
        file.write_all(line.as_bytes())
            .with_context(|| format!("failed to append {}", self.events_path.display()))?;
        Ok(())
    }
}

/// Read every event currently persisted under `<run_root>/lanes/*/events.jsonl`.
/// Used by `auto parallel watch` for both the initial dump and follow-up tails.
pub(crate) fn read_all_events(run_root: &Path) -> Result<Vec<LaneEventEnvelope>> {
    let lanes_root = run_root.join("lanes");
    let mut all = Vec::new();
    if !lanes_root.exists() {
        return Ok(all);
    }
    let entries = std::fs::read_dir(&lanes_root)
        .with_context(|| format!("failed to read {}", lanes_root.display()))?;
    for entry in entries {
        let entry = entry.with_context(|| format!("failed to read {}", lanes_root.display()))?;
        if !entry.file_type()?.is_dir() {
            continue;
        }
        let events_path = entry.path().join(LANE_EVENTS_FILE);
        if !events_path.exists() {
            continue;
        }
        let body = std::fs::read_to_string(&events_path)
            .with_context(|| format!("failed to read {}", events_path.display()))?;
        for line in body.lines() {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            match serde_json::from_str::<LaneEventEnvelope>(line) {
                Ok(env) => all.push(env),
                Err(err) => {
                    eprintln!(
                        "warning: skipping malformed lane event in {}: {err:#}",
                        events_path.display()
                    );
                }
            }
        }
    }
    all.sort_by(|a, b| a.timestamp.cmp(&b.timestamp));
    Ok(all)
}

/// Render a single envelope for display in `auto parallel watch`.
pub(crate) fn render_for_terminal(envelope: &LaneEventEnvelope) -> String {
    let task = envelope.task_id.as_deref().unwrap_or("-");
    let detail = match &envelope.event {
        LaneEvent::TaskStarted => "task started".to_string(),
        LaneEvent::TaskCompleted { outcome } => format!("task completed [{outcome}]"),
        LaneEvent::LaneIdle { summary } => format!("idle: {summary}"),
        LaneEvent::ReceiptDrift { reason } => format!("receipt drift: {reason}"),
        LaneEvent::HostMessage { message } => message.clone(),
    };
    format!(
        "[{ts} lane-{lane:<2} {task:<24}] {detail}",
        ts = envelope.timestamp,
        lane = envelope.lane_index,
        task = task,
    )
}

/// Used by `parallel_command` host emitters that already format a flat string;
/// the same string is mirrored into the structured stream as a `host_message`.
pub(crate) fn classify_host_event(message: &str) -> LaneEvent {
    let trimmed = message.trim();
    if let Some(rest) = trimmed.strip_prefix("idle:") {
        return LaneEvent::LaneIdle {
            summary: rest.trim().to_string(),
        };
    }
    LaneEvent::HostMessage {
        message: trimmed.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn unique_temp_dir(label: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!("autodev-events-{label}-{nanos}"));
        fs::create_dir_all(&path).expect("create temp dir");
        path
    }

    #[test]
    fn emits_and_reads_back_envelopes_in_order() {
        let tmp = unique_temp_dir("roundtrip");
        let run_root = tmp.join("run");
        let lane_one = run_root.join("lanes/lane-1");
        let lane_two = run_root.join("lanes/lane-2");
        fs::create_dir_all(&lane_one).unwrap();
        fs::create_dir_all(&lane_two).unwrap();

        let logger_one = LaneEventLogger::for_lane(&lane_one, 1);
        let logger_two = LaneEventLogger::for_lane(&lane_two, 2);

        logger_one.emit(Some("TASK-1"), LaneEvent::TaskStarted);
        logger_two.emit(Some("TASK-2"), LaneEvent::TaskStarted);
        logger_one.emit(
            Some("TASK-1"),
            LaneEvent::HostMessage {
                message: "compaction triggered by codex".to_string(),
            },
        );
        logger_one.emit(
            Some("TASK-1"),
            LaneEvent::TaskCompleted {
                outcome: "landed".into(),
            },
        );

        let events = read_all_events(&run_root).expect("read");
        assert_eq!(events.len(), 4);

        let lane_one_events: Vec<_> = events.iter().filter(|e| e.lane_index == 1).collect();
        assert_eq!(lane_one_events.len(), 3);
        assert!(matches!(
            lane_one_events[0].event,
            LaneEvent::TaskStarted { .. }
        ));
        assert!(matches!(
            lane_one_events[2].event,
            LaneEvent::TaskCompleted { .. }
        ));

        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn classify_host_event_recognizes_idle_prefix() {
        let event = classify_host_event("idle: queue empty");
        assert_eq!(
            event,
            LaneEvent::LaneIdle {
                summary: "queue empty".to_string()
            }
        );
        let event = classify_host_event("rebased onto origin/main");
        assert_eq!(
            event,
            LaneEvent::HostMessage {
                message: "rebased onto origin/main".to_string()
            }
        );
    }
}
