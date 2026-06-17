//! `auto orchestrate`: a Definition-of-Done acceptance gate.
//!
//! Reads a DoD spec (criteria, each with a `verify` shell command), runs each
//! criterion, computes a "% to done", and emits a live dashboard artifact
//! (`.auto/orchestrate/dod.json`, the shape the orchestration-dashboard
//! consumes) plus a human `DOD-STATUS.md`. With `--execute` it drives
//! `auto loop` against the repo, re-assessing after each landed pass, until
//! every criterion is met or the loop budget is exhausted.
//!
//! `auto super` gates the generated *plan*; `auto orchestrate` gates the *repo
//! against a stated DoD* and loops the existing engine until that DoD holds.

use std::path::Path;
use std::process::Command;

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};

use crate::loop_command::run_loop;
use crate::util::{atomic_write, git_repo_root};
use crate::{LoopArgs, OrchestrateArgs};

const DEFAULT_DOD_REL: &str = ".auto/dod.json";
const STATUS_MD: &str = "DOD-STATUS.md";
const FOCUS_MD: &str = "DOD-FOCUS.md";
const OPERATOR_GATE: &str = "operator";

#[derive(Debug, Deserialize)]
struct DodSpec {
    statement: String,
    criteria: Vec<DodCriterionSpec>,
}

#[derive(Debug, Deserialize)]
struct DodCriterionSpec {
    label: String,
    /// Shell command proving the criterion. Exit 0 => done.
    #[serde(default)]
    verify: Option<String>,
    /// `"operator"` => not auto-verifiable; surfaced as blocked and excluded
    /// from the % denominator (honest about what a tool cannot self-certify).
    #[serde(default)]
    gate: Option<String>,
    #[serde(default)]
    note: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct AssessedCriterion {
    label: String,
    status: String, // "done" | "todo" | "blocked"
    note: String,
}

#[derive(Debug, Clone, Serialize)]
struct AssessedDod {
    statement: String,
    criteria: Vec<AssessedCriterion>,
}

/// Outcome of running one verify command.
struct VerifyOutcome {
    success: bool,
    tail: String,
}

/// Done / total / blocked counts and the resulting percentage.
struct DodScore {
    pct: u32,
    done: usize,
    total: usize,
    blocked: usize,
}

fn is_operator_gated(c: &DodCriterionSpec) -> bool {
    c.gate.as_deref() == Some(OPERATOR_GATE)
}

/// Assess every criterion using `runner` to execute verify commands. Pure with
/// respect to process execution so tests can inject a deterministic runner.
fn assess_with<R: Fn(&str) -> VerifyOutcome>(spec: &DodSpec, runner: &R) -> AssessedDod {
    let criteria = spec
        .criteria
        .iter()
        .map(|c| assess_one(c, runner))
        .collect();
    AssessedDod {
        statement: spec.statement.clone(),
        criteria,
    }
}

fn assess_one<R: Fn(&str) -> VerifyOutcome>(c: &DodCriterionSpec, runner: &R) -> AssessedCriterion {
    if is_operator_gated(c) {
        return AssessedCriterion {
            label: c.label.clone(),
            status: "blocked".to_string(),
            note: c
                .note
                .clone()
                .unwrap_or_else(|| "operator-gated".to_string()),
        };
    }
    match c
        .verify
        .as_deref()
        .map(str::trim)
        .filter(|cmd| !cmd.is_empty())
    {
        Some(cmd) => {
            let outcome = runner(cmd);
            let (status, note) = if outcome.success {
                ("done", c.note.clone().unwrap_or_else(|| cmd.to_string()))
            } else if outcome.tail.is_empty() {
                ("todo", format!("FAILED: {cmd}"))
            } else {
                ("todo", format!("FAILED: {cmd} — {}", outcome.tail))
            };
            AssessedCriterion {
                label: c.label.clone(),
                status: status.to_string(),
                note,
            }
        }
        None => AssessedCriterion {
            label: c.label.clone(),
            status: "todo".to_string(),
            note: c
                .note
                .clone()
                .unwrap_or_else(|| "no verify command".to_string()),
        },
    }
}

/// Percentage over non-blocked criteria; a `done` criterion counts as 1.
fn score(assessed: &AssessedDod) -> DodScore {
    let total = assessed.criteria.len();
    let blocked = assessed
        .criteria
        .iter()
        .filter(|c| c.status == "blocked")
        .count();
    let done = assessed
        .criteria
        .iter()
        .filter(|c| c.status == "done")
        .count();
    let denom = total.saturating_sub(blocked);
    let pct = if denom == 0 {
        100
    } else {
        ((done as f64) * 100.0 / denom as f64).round() as u32
    };
    DodScore {
        pct,
        done,
        total,
        blocked,
    }
}

/// DoD is met when nothing is left to do (no `todo` criterion remains).
fn is_met(assessed: &AssessedDod) -> bool {
    !assessed.criteria.iter().any(|c| c.status == "todo")
}

fn status_symbol(status: &str) -> &'static str {
    match status {
        "done" => "✓",
        "blocked" => "🔒",
        _ => "○",
    }
}

fn render_status_md(assessed: &AssessedDod, s: &DodScore) -> String {
    let mut out = String::from("# Definition of Done — status\n\n");
    out.push_str(&format!(
        "**{}% to done** — {}/{} criteria met",
        s.pct,
        s.done,
        s.total - s.blocked
    ));
    if s.blocked > 0 {
        out.push_str(&format!(" ({} operator-gated, excluded)", s.blocked));
    }
    out.push_str("\n\n> ");
    out.push_str(&assessed.statement);
    out.push_str("\n\n");
    for c in &assessed.criteria {
        out.push_str(&format!(
            "- {} {} — {}\n",
            status_symbol(&c.status),
            c.label,
            c.note
        ));
    }
    out
}

fn tail_lines(text: &str, n: usize) -> String {
    let lines: Vec<&str> = text.lines().filter(|l| !l.trim().is_empty()).collect();
    let start = lines.len().saturating_sub(n);
    lines[start..].join(" | ")
}

fn run_verify(repo_root: &Path, cmd: &str, timeout_secs: u64) -> VerifyOutcome {
    let output = Command::new("timeout")
        .arg(format!("{timeout_secs}s"))
        .arg("bash")
        .arg("-lc")
        .arg(cmd)
        .current_dir(repo_root)
        .output();
    match output {
        Ok(out) => {
            let mut combined = String::from_utf8_lossy(&out.stdout).into_owned();
            combined.push_str(&String::from_utf8_lossy(&out.stderr));
            VerifyOutcome {
                success: out.status.success(),
                tail: tail_lines(&combined, 3),
            }
        }
        Err(e) => VerifyOutcome {
            success: false,
            tail: format!("spawn error: {e}"),
        },
    }
}

fn load_spec(path: &Path) -> Result<DodSpec> {
    let text = std::fs::read_to_string(path)
        .with_context(|| format!("failed to read DoD spec {}", path.display()))?;
    let spec: DodSpec = serde_json::from_str(&text)
        .with_context(|| format!("DoD spec {} is not valid JSON", path.display()))?;
    if spec.criteria.is_empty() {
        bail!("DoD spec {} has no criteria", path.display());
    }
    Ok(spec)
}

fn emit(
    repo_root: &Path,
    repo_name: &str,
    assessed: &AssessedDod,
    status_md: &str,
    dashboard: Option<&Path>,
) -> Result<()> {
    let auto_dir = repo_root.join(".auto").join("orchestrate");
    std::fs::create_dir_all(&auto_dir)
        .with_context(|| format!("failed to create {}", auto_dir.display()))?;
    let dod_json = serde_json::to_vec_pretty(assessed)?;
    atomic_write(&auto_dir.join("dod.json"), &dod_json)?;
    atomic_write(&repo_root.join(STATUS_MD), status_md.as_bytes())?;
    if let Some(dir) = dashboard {
        std::fs::create_dir_all(dir)
            .with_context(|| format!("failed to create {}", dir.display()))?;
        // Per-repo keyed file the shared dashboard can merge by name.
        atomic_write(&dir.join(format!("{repo_name}.dod.json")), &dod_json)?;
    }
    Ok(())
}

fn write_focus(repo_root: &Path, assessed: &AssessedDod) -> Result<()> {
    let mut out = String::from(
        "# DoD focus — unmet criteria\n\nDrive the implementation toward closing these:\n\n",
    );
    for c in assessed.criteria.iter().filter(|c| c.status == "todo") {
        out.push_str(&format!("- {} — {}\n", c.label, c.note));
    }
    atomic_write(&repo_root.join(FOCUS_MD), out.as_bytes())
}

fn build_loop_args(args: &OrchestrateArgs) -> LoopArgs {
    LoopArgs {
        max_iterations: Some(1),
        prompt_file: None,
        model: args.model.clone(),
        reasoning_effort: args.reasoning_effort.clone(),
        branch: args.branch.clone(),
        reference_repos: Vec::new(),
        include_siblings: false,
        run_root: None,
        codex_bin: args.codex_bin.clone(),
        claude: false,
        max_turns: None,
        max_retries: 2,
    }
}

fn print_assessment(assessed: &AssessedDod, s: &DodScore) {
    println!(
        "assessed:    {}% to done ({}/{} met{})",
        s.pct,
        s.done,
        s.total - s.blocked,
        if s.blocked > 0 {
            format!(", {} gated", s.blocked)
        } else {
            String::new()
        }
    );
    for c in &assessed.criteria {
        println!("  {} {}", status_symbol(&c.status), c.label);
    }
}

pub(crate) async fn run_orchestrate(args: OrchestrateArgs) -> Result<()> {
    let repo_root = git_repo_root()?;
    let repo_name = repo_root
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("repo")
        .to_string();
    let dod_path = args
        .dod
        .clone()
        .unwrap_or_else(|| repo_root.join(DEFAULT_DOD_REL));
    let spec = load_spec(&dod_path)?;

    println!("auto orchestrate");
    println!("repo:        {}", repo_root.display());
    println!("dod:         {}", dod_path.display());
    println!("criteria:    {}", spec.criteria.len());
    println!(
        "mode:        {}",
        if args.execute {
            "assess + execute"
        } else {
            "assess only"
        }
    );

    let runner = |cmd: &str| run_verify(&repo_root, cmd, args.verify_timeout_secs);
    let mut assessed = assess_with(&spec, &runner);
    let mut s = score(&assessed);
    emit(
        &repo_root,
        &repo_name,
        &assessed,
        &render_status_md(&assessed, &s),
        args.dashboard.as_deref(),
    )?;
    print_assessment(&assessed, &s);

    if args.execute && !is_met(&assessed) {
        let mut last_pct = s.pct;
        let mut stagnant = 0u32;
        for pass in 1..=args.max_loops {
            write_focus(&repo_root, &assessed)?;
            println!("\n── execute pass {pass}/{} (auto loop) ──", args.max_loops);
            run_loop(build_loop_args(&args))
                .await
                .with_context(|| format!("execution pass {pass} failed"))?;

            assessed = assess_with(&spec, &runner);
            s = score(&assessed);
            emit(
                &repo_root,
                &repo_name,
                &assessed,
                &render_status_md(&assessed, &s),
                args.dashboard.as_deref(),
            )?;
            print_assessment(&assessed, &s);

            if is_met(&assessed) {
                println!("DoD met after {pass} pass(es).");
                break;
            }
            if s.pct <= last_pct {
                stagnant += 1;
            } else {
                stagnant = 0;
            }
            last_pct = s.pct;
            if stagnant >= 2 {
                println!("no progress for 2 consecutive passes — stopping.");
                break;
            }
        }
    }

    println!("\nstatus:      {}", repo_root.join(STATUS_MD).display());
    if is_met(&assessed) {
        println!("DoD: MET ✓");
        Ok(())
    } else {
        println!("DoD: NOT MET ({}%)", s.pct);
        std::process::exit(2);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec_json(extra: &str) -> DodSpec {
        let text = format!(
            r#"{{ "statement": "A real user can do the thing.", "criteria": [ {extra} ] }}"#
        );
        serde_json::from_str(&text).expect("valid spec json")
    }

    fn ok(_cmd: &str) -> VerifyOutcome {
        VerifyOutcome {
            success: true,
            tail: String::new(),
        }
    }

    #[test]
    fn assess_marks_passing_command_done_and_failing_todo() {
        let spec = spec_json(r#"{"label":"a","verify":"true"}, {"label":"b","verify":"false"}"#);
        let runner = |cmd: &str| VerifyOutcome {
            success: cmd == "true",
            tail: if cmd == "true" {
                String::new()
            } else {
                "boom".to_string()
            },
        };
        let assessed = assess_with(&spec, &runner);
        assert_eq!(assessed.criteria[0].status, "done");
        assert_eq!(assessed.criteria[1].status, "todo");
        assert!(assessed.criteria[1].note.contains("FAILED"));
        assert!(assessed.criteria[1].note.contains("boom"));
    }

    #[test]
    fn assess_marks_operator_gated_blocked_without_running() {
        let spec = spec_json(
            r#"{"label":"creds","gate":"operator","note":"user supplies key","verify":"false"}"#,
        );
        // Runner that would panic proves the gated criterion never runs.
        let runner = |_cmd: &str| -> VerifyOutcome { panic!("must not run gated verify") };
        let assessed = assess_with(&spec, &runner);
        assert_eq!(assessed.criteria[0].status, "blocked");
        assert_eq!(assessed.criteria[0].note, "user supplies key");
    }

    #[test]
    fn criterion_without_verify_is_todo() {
        let spec = spec_json(r#"{"label":"a"}"#);
        let assessed = assess_with(&spec, &ok);
        assert_eq!(assessed.criteria[0].status, "todo");
    }

    #[test]
    fn score_excludes_blocked_from_denominator() {
        let spec = spec_json(
            r#"{"label":"a","verify":"true"}, {"label":"b","verify":"false"}, {"label":"c","gate":"operator"}"#,
        );
        let runner = |cmd: &str| VerifyOutcome {
            success: cmd == "true",
            tail: String::new(),
        };
        let s = score(&assess_with(&spec, &runner));
        // 1 done of 2 non-blocked = 50%; 3 total, 1 blocked.
        assert_eq!(s.pct, 50);
        assert_eq!(s.done, 1);
        assert_eq!(s.total, 3);
        assert_eq!(s.blocked, 1);
    }

    #[test]
    fn is_met_true_when_no_todo_remains() {
        let spec = spec_json(r#"{"label":"a","verify":"true"}, {"label":"c","gate":"operator"}"#);
        let assessed = assess_with(&spec, &ok);
        assert!(is_met(&assessed));
        let s = score(&assessed);
        assert_eq!(s.pct, 100);
    }

    #[test]
    fn is_met_false_with_a_todo() {
        let spec = spec_json(r#"{"label":"a","verify":"false"}"#);
        let assessed = assess_with(&spec, &|_| VerifyOutcome {
            success: false,
            tail: String::new(),
        });
        assert!(!is_met(&assessed));
    }

    #[test]
    fn status_md_reports_pct_and_symbols() {
        let spec = spec_json(
            r#"{"label":"alpha","verify":"true"}, {"label":"beta","verify":"false"}, {"label":"gamma","gate":"operator"}"#,
        );
        let runner = |cmd: &str| VerifyOutcome {
            success: cmd == "true",
            tail: String::new(),
        };
        let assessed = assess_with(&spec, &runner);
        let md = render_status_md(&assessed, &score(&assessed));
        assert!(md.contains("50% to done"));
        assert!(md.contains("✓ alpha"));
        assert!(md.contains("○ beta"));
        assert!(md.contains("🔒 gamma"));
        assert!(md.contains("operator-gated, excluded"));
    }

    #[test]
    fn tail_lines_keeps_last_nonempty() {
        let text = "one\n\ntwo\nthree\n\n";
        assert_eq!(tail_lines(text, 2), "two | three");
    }

    #[test]
    fn load_spec_rejects_empty_criteria() {
        let dir = std::env::temp_dir().join(format!(
            "orch-spec-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("dod.json");
        std::fs::write(&path, r#"{"statement":"x","criteria":[]}"#).unwrap();
        assert!(load_spec(&path).is_err());
        std::fs::remove_dir_all(&dir).ok();
    }
}
