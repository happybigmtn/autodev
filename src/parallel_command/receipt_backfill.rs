use super::*;

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ReceiptBackfillEntry {
    pub(crate) task_id: String,
    pub(crate) title: String,
    pub(crate) missing_reasons: Vec<String>,
    pub(crate) review_handoff_missing: bool,
    pub(crate) verification_commands: Vec<String>,
    pub(crate) wrapper_commands: Vec<String>,
}

pub(crate) fn run_parallel_receipt_backfill(args: &ParallelArgs) -> Result<()> {
    let repo_root = git_repo_root()?;
    ensure_repo_layout(&repo_root)?;
    let run_root = parallel_run_root(&repo_root, args);
    ensure_writable_run_root(&run_root)?;
    let _repo_handoff_lease = if args.apply_receipt_backfill_handoffs {
        refuse_live_parallel_host_for_backfill(&repo_root)?;
        Some(acquire_parallel_repo_lease(&repo_root, "receipt-backfill")?)
    } else {
        None
    };
    let _run_handoff_lease = if args.apply_receipt_backfill_handoffs {
        Some(acquire_parallel_host_lease(&run_root, "receipt-backfill")?)
    } else {
        None
    };
    if args.apply_receipt_backfill_handoffs {
        // Re-probe after both modern-host leases close the startup race and
        // continue to detect older installed hosts that do not take leases.
        refuse_live_parallel_host_for_backfill(&repo_root)?;
    }

    let plan_text = read_loop_plan(&repo_root)?;
    let snapshot = parse_loop_plan(&plan_text);
    let mut entries = receipt_backfill_entries(&repo_root, &plan_text, &snapshot);
    let mut applied_handoffs = Vec::new();

    if args.apply_receipt_backfill_handoffs {
        refuse_dirty_review_before_backfill(&repo_root)?;
        for task in snapshot
            .tasks
            .iter()
            .filter(|task| task.status == LoopTaskStatus::Done)
        {
            let evidence = inspect_task_completion_evidence(&repo_root, &task.id, &task.markdown);
            if evidence.has_review_handoff {
                continue;
            }
            let mut review_evidence = evidence.clone();
            review_evidence.has_review_handoff = true;
            if ensure_host_review_handoff(
                &repo_root,
                &task.id,
                &review_handoff_changed_files(&review_evidence),
                &review_evidence,
            )? {
                applied_handoffs.push(task.id.clone());
            }
        }
        if !applied_handoffs.is_empty() {
            run_git(&repo_root, ["add", "REVIEW.md"])?;
            entries = receipt_backfill_entries(&repo_root, &plan_text, &snapshot);
        }
    }

    let report = render_receipt_backfill_report(
        &repo_root,
        args.apply_receipt_backfill_handoffs,
        &applied_handoffs,
        &entries,
    );
    let report_path = run_root.join("receipt-backfill.md");
    atomic_write(&report_path, report.as_bytes())
        .with_context(|| format!("failed to write {}", report_path.display()))?;

    println!("receipt backfill report: {}", report_path.display());
    if entries.is_empty() {
        println!("receipt backfill: no completed task drift detected");
    } else {
        println!(
            "receipt backfill: {} completed task(s) need evidence repair",
            entries.len()
        );
        if !applied_handoffs.is_empty() {
            println!(
                "receipt backfill: applied REVIEW.md handoff(s) for {}",
                applied_handoffs.join(", ")
            );
            println!("receipt backfill: REVIEW.md is staged; commit after review");
        }
    }
    Ok(())
}

fn refuse_live_parallel_host_for_backfill(repo_root: &Path) -> Result<()> {
    let session = parallel_tmux_session_name(repo_root);
    let tmux_running = tmux_session_exists(&session)
        .with_context(|| format!("cannot prove parallel host `{session}` is stopped"))?;
    let host_processes = parallel_host_processes_for_repo_strict(repo_root)
        .context("cannot prove no direct parallel host is running")?;
    if parallel_prune_host_is_active(tmux_running, &host_processes) {
        bail!(
            "refusing receipt-backfill handoff apply while a parallel host is active for {}",
            repo_root.display()
        );
    }
    Ok(())
}

fn refuse_dirty_review_before_backfill(repo_root: &Path) -> Result<()> {
    let status = git_stdout(repo_root, ["status", "--porcelain=v1", "--", "REVIEW.md"])?;
    if !status.trim().is_empty() {
        bail!(
            "refusing receipt-backfill handoff apply because REVIEW.md already has staged or unstaged changes; commit or stash them first"
        );
    }
    Ok(())
}

pub(crate) fn receipt_backfill_entries(
    repo_root: &Path,
    plan_text: &str,
    snapshot: &LoopPlanSnapshot,
) -> Vec<ReceiptBackfillEntry> {
    let all_plan_tasks = parse_shared_tasks(plan_text);
    let receipt_footers = git_verification_receipt_footers(repo_root);
    snapshot
        .tasks
        .iter()
        .filter(|task| task.status == LoopTaskStatus::Done)
        .filter_map(|task| {
            let current_fingerprint =
                compute_task_owned_inputs_fingerprint(repo_root, &task.id, &all_plan_tasks);
            let unchanged_owned_inputs = matching_owned_inputs_fingerprint(
                &task.id,
                &receipt_footers,
                current_fingerprint.as_deref(),
            );
            let evidence = inspect_task_completion_evidence_with_owned_inputs(
                repo_root,
                &task.id,
                &task.markdown,
                unchanged_owned_inputs,
            );
            if evidence.is_fully_evidenced() {
                return None;
            }
            let verification = verification_plan(&task.markdown);
            let wrapper_commands = verification
                .executable_commands
                .iter()
                .map(|command| render_receipt_wrapper_command(&task.id, command))
                .collect::<Vec<_>>();
            Some(ReceiptBackfillEntry {
                task_id: task.id.clone(),
                title: task.title.clone(),
                missing_reasons: evidence.missing_reasons(),
                review_handoff_missing: !evidence.has_review_handoff,
                verification_commands: verification.executable_commands,
                wrapper_commands,
            })
        })
        .collect()
}

fn matching_owned_inputs_fingerprint<'a>(
    task_id: &str,
    receipt_footers: &[VerificationReceiptFooter],
    current_fingerprint: Option<&'a str>,
) -> Option<&'a str> {
    let current = current_fingerprint?;
    let stored = receipt_footers
        .iter()
        .find(|footer| footer.task_id == task_id)
        .and_then(footer_task_owned_inputs)?;
    (stored == current).then_some(current)
}

pub(crate) fn render_receipt_backfill_report(
    repo_root: &Path,
    apply_handoffs: bool,
    applied_handoffs: &[String],
    entries: &[ReceiptBackfillEntry],
) -> String {
    let wrapper_present = repo_root.join("scripts/run-task-verification.sh").exists();
    let recorder_present = repo_root.join("scripts/verification_receipt.py").exists();
    let mut report = String::new();
    report.push_str("# Parallel Receipt Backfill\n\n");
    report.push_str("Generated by `auto parallel receipt-backfill`.\n\n");
    report.push_str("## Preconditions\n\n");
    report.push_str(&format!(
        "- `scripts/run-task-verification.sh`: {}\n",
        present_label(wrapper_present)
    ));
    report.push_str(&format!(
        "- `scripts/verification_receipt.py`: {}\n",
        present_label(recorder_present)
    ));
    report.push_str(&format!(
        "- apply review handoffs: {}\n",
        if apply_handoffs { "yes" } else { "no" }
    ));
    if !applied_handoffs.is_empty() {
        report.push_str(&format!(
            "- applied handoffs: {}\n",
            applied_handoffs.join(", ")
        ));
    }
    report.push('\n');

    if entries.is_empty() {
        report.push_str("No completed task drift detected.\n");
        return report;
    }

    report.push_str("## Tasks Needing Backfill\n\n");
    for entry in entries {
        report.push_str(&format!("### `{}` {}\n\n", entry.task_id, entry.title));
        report.push_str(&format!(
            "- review handoff: {}\n",
            if entry.review_handoff_missing {
                "missing"
            } else {
                "present"
            }
        ));
        if entry.missing_reasons.is_empty() {
            report.push_str("- missing evidence: none reported\n");
        } else {
            report.push_str("- missing evidence:\n");
            for reason in &entry.missing_reasons {
                report.push_str(&format!("  - {reason}\n"));
            }
        }
        if entry.verification_commands.is_empty() {
            report.push_str("- executable verification: none declared\n\n");
            continue;
        }
        report.push_str("- receipt commands:\n\n");
        report.push_str("```bash\n");
        for command in &entry.wrapper_commands {
            report.push_str(command);
            report.push('\n');
        }
        report.push_str("```\n\n");
    }
    report.push_str("## Reconcile After Backfill\n\n");
    report.push_str("After review handoffs and receipt commands are complete, run:\n\n");
    report.push_str("```bash\n");
    report.push_str("AUTO_PARALLEL_TMUX_BOOTSTRAPPED=1 AUTO_SKIP_REMOTE_SYNC=1 auto parallel --max-iterations 0 --threads 2\n");
    report.push_str("```\n");
    report
}

fn present_label(present: bool) -> &'static str {
    if present {
        "present"
    } else {
        "missing"
    }
}

fn review_handoff_changed_files(
    evidence: &crate::completion_artifacts::TaskCompletionEvidence,
) -> Vec<String> {
    if evidence.declared_completion_artifacts.is_empty() {
        Vec::new()
    } else {
        evidence.declared_completion_artifacts.clone()
    }
}

pub(crate) fn render_receipt_wrapper_command(task_id: &str, command: &str) -> String {
    let normalized = normalize_receipt_command(command);
    format!("scripts/run-task-verification.sh {task_id} -- {normalized}")
}

pub(crate) fn normalize_receipt_command(command: &str) -> String {
    let trimmed = command.trim();
    if command_is_shell_harness(trimmed) || !command_needs_shell(trimmed) {
        trimmed.to_string()
    } else {
        format!("bash -lc {}", shell_single_quote(trimmed))
    }
}

fn command_is_shell_harness(command: &str) -> bool {
    command == "bash -lc" || command.starts_with("bash -lc ")
}

fn command_needs_shell(command: &str) -> bool {
    let mut chars = command.chars().peekable();
    while let Some(ch) = chars.next() {
        match ch {
            '|' | ';' | '<' | '>' => return true,
            '&' if chars.peek() == Some(&'&') => return true,
            _ => {}
        }
    }
    false
}

fn shell_single_quote(command: &str) -> String {
    format!("'{}'", command.replace('\'', r#"'"'"'"#))
}

#[cfg(test)]
mod tests {
    use super::{
        matching_owned_inputs_fingerprint, normalize_receipt_command, receipt_backfill_entries,
        refuse_dirty_review_before_backfill, render_receipt_backfill_report,
        render_receipt_wrapper_command, ReceiptBackfillEntry,
    };
    use crate::completion_artifacts::{
        current_dirty_state_fingerprint, normalized_plan_hash_bytes,
        record_verified_source_attestation, verification_receipt_commit_footer,
        VerificationReceiptFooter,
    };
    use std::fs;
    use std::path::Path;
    use std::process::Command;

    fn git(root: &Path, args: &[&str]) {
        let status = Command::new("git")
            .arg("-C")
            .arg(root)
            .args(args)
            .status()
            .expect("run git");
        assert!(status.success(), "git command failed: {args:?}");
    }

    fn temp_repo(label: &str) -> std::path::PathBuf {
        let root = std::env::temp_dir().join(format!(
            "autodev-{label}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock")
                .as_nanos()
        ));
        fs::create_dir_all(&root).expect("create temp repo");
        git(&root, &["init", "-q"]);
        git(&root, &["config", "user.email", "autodev@example.invalid"]);
        git(&root, &["config", "user.name", "Autodev Test"]);
        root
    }

    #[test]
    fn wrapper_command_normalizes_shell_operators() {
        assert_eq!(
            render_receipt_wrapper_command("TASK-1", "rg -n \"secret\" src || true"),
            "scripts/run-task-verification.sh TASK-1 -- bash -lc 'rg -n \"secret\" src || true'"
        );
        assert_eq!(
            normalize_receipt_command("bash -lc 'rg -n \"secret\" src || true'"),
            "bash -lc 'rg -n \"secret\" src || true'"
        );
    }

    #[test]
    fn backfill_handoff_apply_refuses_preexisting_review_changes() {
        let root = temp_repo("receipt-backfill-dirty-review");
        fs::write(root.join("REVIEW.md"), "# Review\n").unwrap();
        git(&root, &["add", "REVIEW.md"]);
        git(&root, &["commit", "-q", "-m", "seed review"]);

        fs::write(root.join("REVIEW.md"), "# Review\n\nuser work\n").unwrap();
        let error = refuse_dirty_review_before_backfill(&root).unwrap_err();
        assert!(error.to_string().contains("REVIEW.md already has"));

        let staged = Command::new("git")
            .arg("-C")
            .arg(&root)
            .args(["diff", "--cached", "--name-only"])
            .output()
            .unwrap();
        assert!(staged.stdout.is_empty(), "user edits must not be staged");

        git(&root, &["add", "REVIEW.md"]);
        let staged_error = refuse_dirty_review_before_backfill(&root).unwrap_err();
        assert!(staged_error.to_string().contains("REVIEW.md already has"));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn backfill_handoff_apply_refuses_preexisting_untracked_review() {
        let root = temp_repo("receipt-backfill-untracked-review");
        fs::write(root.join("REVIEW.md"), "user review\n").unwrap();
        let error = refuse_dirty_review_before_backfill(&root).unwrap_err();
        assert!(error.to_string().contains("REVIEW.md already has"));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn backfill_trusts_matching_task_owned_inputs_only() {
        let footers = vec![VerificationReceiptFooter {
            task_id: "TASK-1".to_string(),
            commit: "ancestor".to_string(),
            receipt_text: r#"{"task_owned_inputs_v2":"owned-v2"}"#.to_string(),
        }];

        assert_eq!(
            matching_owned_inputs_fingerprint("TASK-1", &footers, Some("owned-v2")),
            Some("owned-v2")
        );
        assert_eq!(
            matching_owned_inputs_fingerprint("TASK-1", &footers, Some("changed")),
            None
        );
        assert_eq!(
            matching_owned_inputs_fingerprint("TASK-2", &footers, Some("owned-v2")),
            None
        );
    }

    #[test]
    fn backfill_prefers_v2_and_does_not_treat_legacy_v1_as_current() {
        let v1 = VerificationReceiptFooter {
            task_id: "TASK-1".to_string(),
            commit: "ancestor".to_string(),
            receipt_text: r#"{"task_owned_inputs_v1":"legacy-v1"}"#.to_string(),
        };
        assert_eq!(
            matching_owned_inputs_fingerprint("TASK-1", &[v1], Some("current-v2")),
            None
        );

        let both = VerificationReceiptFooter {
            task_id: "TASK-1".to_string(),
            commit: "ancestor".to_string(),
            receipt_text:
                r#"{"task_owned_inputs_v1":"legacy-v1","task_owned_inputs_v2":"current-v2"}"#
                    .to_string(),
        };
        assert_eq!(
            matching_owned_inputs_fingerprint("TASK-1", &[both], Some("current-v2")),
            Some("current-v2")
        );
    }

    #[test]
    fn unrelated_head_advance_does_not_backfill_task_scoped_receipt() {
        let root = temp_repo("receipt-backfill-owned-inputs");
        let partial_plan = "- [~] `TASK-1` Task scoped proof\n  Owns: `src/task.rs`\n  Verification: `cargo test task_one`\n  Completion artifacts: none\n  Dependencies: none\n";
        let done_plan = partial_plan.replacen("- [~]", "- [x]", 1);
        fs::create_dir_all(root.join("src")).expect("create src");
        fs::create_dir_all(root.join("scripts")).expect("create scripts");
        fs::write(root.join("src/task.rs"), "pub fn task() {}\n").expect("write source");
        fs::write(root.join("scripts/run-task-verification.sh"), "#!/bin/sh\n")
            .expect("write wrapper");
        fs::write(
            root.join("REVIEW.md"),
            "# REVIEW\n\nAwaiting auto review:\n## `TASK-1`\n",
        )
        .expect("write review");
        fs::write(root.join("PLAN.md"), partial_plan).expect("write plan");
        git(&root, &["add", "-A"]);
        git(&root, &["commit", "-q", "-m", "seed partial task"]);

        let commit = Command::new("git")
            .arg("-C")
            .arg(&root)
            .args(["rev-parse", "HEAD"])
            .output()
            .expect("read head");
        let commit = String::from_utf8(commit.stdout)
            .expect("utf8 head")
            .trim()
            .to_string();
        let dirty = current_dirty_state_fingerprint(&root).expect("dirty fingerprint");
        let plan_hash = normalized_plan_hash_bytes(partial_plan.as_bytes());
        fs::create_dir_all(root.join(".auto/symphony/verification-receipts"))
            .expect("create receipt dir");
        fs::write(
            root.join(".auto/symphony/verification-receipts/TASK-1.json"),
            format!(
                r#"{{"task_id":"TASK-1","commit":"{commit}","dirty_state":{{"fingerprint":"{dirty}"}},"plan_hash":"{plan_hash}","commands":[{{"command":"cargo test task_one","argv":["cargo","test","task_one"],"expected_argv":["cargo","test","task_one"],"exit_code":0,"status":"passed"}}]}}"#
            ),
        )
        .expect("write receipt");
        record_verified_source_attestation(&root, "TASK-1").expect("attest source");
        fs::write(root.join("PLAN.md"), &done_plan).expect("mark done");
        let footer = verification_receipt_commit_footer(&root, "TASK-1")
            .expect("prepare footer")
            .expect("footer present");
        git(&root, &["add", "PLAN.md"]);
        git(
            &root,
            &[
                "commit",
                "-q",
                "-m",
                "repo: TASK-1 queue sync",
                "-m",
                &footer,
            ],
        );
        fs::remove_file(root.join(".auto/symphony/verification-receipts/TASK-1.json"))
            .expect("remove staging receipt");

        fs::write(root.join("unrelated.txt"), "unrelated\n").expect("write unrelated");
        git(&root, &["add", "unrelated.txt"]);
        git(&root, &["commit", "-q", "-m", "unrelated change"]);
        let snapshot = super::parse_loop_plan(&done_plan);
        assert!(receipt_backfill_entries(&root, &done_plan, &snapshot).is_empty());

        fs::write(
            root.join("src/task.rs"),
            "pub fn task() { /* changed */ }\n",
        )
        .expect("change owned source");
        assert_eq!(
            receipt_backfill_entries(&root, &done_plan, &snapshot).len(),
            1
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn backfill_report_includes_commands_and_reconcile_step() {
        let entry = ReceiptBackfillEntry {
            task_id: "TASK-1".to_string(),
            title: "Example".to_string(),
            missing_reasons: vec!["missing REVIEW.md handoff".to_string()],
            review_handoff_missing: true,
            verification_commands: vec!["cargo test example".to_string()],
            wrapper_commands: vec![
                "scripts/run-task-verification.sh TASK-1 -- cargo test example".to_string(),
            ],
        };

        let report = render_receipt_backfill_report(
            std::path::Path::new("/tmp/no-such-repo"),
            false,
            &[],
            &[entry],
        );

        assert!(report.contains("### `TASK-1` Example"));
        assert!(report.contains("missing REVIEW.md handoff"));
        assert!(report.contains("scripts/run-task-verification.sh TASK-1 -- cargo test example"));
        assert!(report.contains("AUTO_PARALLEL_TMUX_BOOTSTRAPPED=1"));
    }
}
