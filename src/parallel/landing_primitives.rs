fn cherry_pick_lane_range(
    repo_root: &Path,
    base_commit: &str,
    head_ref: &str,
    failure_policy: CherryPickFailurePolicy,
) -> Result<()> {
    if lane_changed_files(repo_root, base_commit, head_ref)?.is_empty() {
        return Ok(());
    }

    scrub_parallel_receipt_staging(repo_root)?;
    let range = format!("{base_commit}..{head_ref}");
    let output = Command::new("git")
        .arg("-C")
        .arg(repo_root)
        .arg("cherry-pick")
        .arg("--empty=drop")
        .arg(&range)
        .output()
        .with_context(|| format!("failed to cherry-pick {range} in {}", repo_root.display()))?;
    if output.status.success() {
        return Ok(());
    }

    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
    let conflicted = cherry_pick_conflicted_paths(repo_root);
    if failure_policy == CherryPickFailurePolicy::Abort {
        let _ = run_git(repo_root, ["cherry-pick", "--abort"]);
    }
    if conflicted.is_empty() {
        bail!(
            "git cherry-pick failed in {}: {stderr}",
            repo_root.display(),
        );
    }
    bail!(
        "git cherry-pick failed in {}: {stderr}; conflicts: {}",
        repo_root.display(),
        conflicted.join(", ")
    );
}

/// Returns the list of paths currently in conflict (unmerged stage entries).
///
/// Empty when there is no active cherry-pick or merge.
pub(crate) fn cherry_pick_conflicted_paths(repo_root: &Path) -> Vec<String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(repo_root)
        .args(["diff", "--name-only", "--diff-filter=U"])
        .output();
    let Ok(output) = output else { return Vec::new() };
    if !output.status.success() {
        return Vec::new();
    }
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(str::to_string)
        .collect()
}

/// Maximum number of consecutive cherry-pick failures (per-lane) before
/// `cherry_pick_lane_range_with_fallback` falls back to rebase + squash
/// merge. Reads `AUTODEV_CHERRY_PICK_FALLBACK_THRESHOLD` if set.
pub(crate) fn cherry_pick_fallback_threshold() -> u32 {
    env::var("AUTODEV_CHERRY_PICK_FALLBACK_THRESHOLD")
        .ok()
        .and_then(|s| s.parse::<u32>().ok())
        .filter(|n| *n > 0)
        .unwrap_or(DEFAULT_CHERRY_PICK_FALLBACK_THRESHOLD)
}

/// Per-lane checkpoint file path. One file per lane directory so a
/// survived process can read it back at resume.
pub(crate) fn lane_checkpoint_path(lane_root: &Path) -> PathBuf {
    lane_root.join("lane-state.json")
}

/// Best-effort lane checkpoint writer. Persisting progress must never
/// fail the lane, so I/O errors are logged to stderr and swallowed.
pub(crate) fn record_lane_checkpoint(
    lane_root: &Path,
    phase: &str,
    blob: serde_json::Value,
) {
    let path = lane_checkpoint_path(lane_root);
    if let Err(err) = write_session_lane_checkpoint(&path, phase, blob) {
        eprintln!(
            "session-survival: failed to write lane checkpoint {} (phase={phase}): {err:#}",
            path.display()
        );
    }
}

/// Read a previously written lane checkpoint if present. Read errors
/// are logged and surfaced as `None` so they cannot wedge resume.
pub(crate) fn load_lane_checkpoint(lane_root: &Path) -> Option<SessionLaneCheckpoint> {
    let path = lane_checkpoint_path(lane_root);
    match read_session_lane_checkpoint(&path) {
        Ok(value) => value,
        Err(err) => {
            eprintln!(
                "session-survival: failed to read lane checkpoint {}: {err:#}",
                path.display()
            );
            None
        }
    }
}

/// Outcome of [`cherry_pick_lane_range_with_fallback`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum CherryPickFallbackOutcome {
    /// The cherry-pick succeeded directly.
    CherryPicked,
    /// The fallback ran successfully — `parent` was restored, lane was
    /// rebased onto `base`, and the lane diff landed as a single squash
    /// commit.
    Squashed { conflicts_seen: u32 },
}

/// Tries cherry-pick first and falls back to rebase + squash merge after
/// `threshold` consecutive merge-conflict failures (Runner-up #90).
///
/// `lane_branch` is the symbolic ref to merge from; `base` is the
/// canonical branch to rebase onto; `parent` is the commit to hard-reset
/// to before the fallback. The fallback path produces a single commit
/// summarising the entire lane diff so we stop walking 61-conflict
/// pre-image cherry-pick stacks.
pub(crate) fn cherry_pick_lane_range_with_fallback(
    repo_root: &Path,
    base: &str,
    parent: &str,
    lane_branch: &str,
    threshold: u32,
) -> Result<CherryPickFallbackOutcome> {
    let mut consecutive_conflicts = 0u32;
    let mut last_conflicts: Vec<String> = Vec::new();
    while consecutive_conflicts < threshold {
        match cherry_pick_lane_range(
            repo_root,
            parent,
            lane_branch,
            CherryPickFailurePolicy::Abort,
        ) {
            Ok(()) => return Ok(CherryPickFallbackOutcome::CherryPicked),
            Err(err) => {
                let message = format!("{err:#}");
                let conflicts = extract_conflict_list(&message);
                if conflicts.is_empty() {
                    return Err(err);
                }
                consecutive_conflicts += 1;
                last_conflicts = conflicts;
            }
        }
    }
    // Threshold reached — fall back to a deterministic squash.
    //
    // We materialise the lane's end-state tree onto `base` as a single
    // commit. This loses interim history (intentional: the operator can
    // still inspect the lane branch ref) but it sidesteps the 61-pre-image
    // conflict loop and produces one auditable commit.
    let lane_sha = git_stdout(repo_root, ["rev-parse", lane_branch])?
        .trim()
        .to_string();
    run_git(repo_root, ["reset", "--hard", base])?;
    // read-tree -u --reset writes lane's tree into the index AND worktree.
    let read_tree = Command::new("git")
        .arg("-C")
        .arg(repo_root)
        .args(["read-tree", "-u", "--reset", &lane_sha])
        .output()
        .with_context(|| format!("failed to launch git in {}", repo_root.display()))?;
    if !read_tree.status.success() {
        bail!(
            "cherry-pick fallback: git read-tree {lane_sha} failed in {}: {}; prior conflicts: {}",
            repo_root.display(),
            String::from_utf8_lossy(&read_tree.stderr).trim(),
            last_conflicts.join(", ")
        );
    }
    let _ = run_git(repo_root, ["add", "-A"]);
    let commit_output = Command::new("git")
        .arg("-C")
        .arg(repo_root)
        .args([
            "commit",
            "--allow-empty",
            "-m",
            &format!(
                "cherry-pick fallback: squash {lane_branch} (resolved {} conflicts)",
                consecutive_conflicts
            ),
        ])
        .output()
        .with_context(|| format!("failed to launch git in {}", repo_root.display()))?;
    if !commit_output.status.success() {
        bail!(
            "cherry-pick fallback: squash commit failed in {}: {}",
            repo_root.display(),
            String::from_utf8_lossy(&commit_output.stderr).trim()
        );
    }
    Ok(CherryPickFallbackOutcome::Squashed {
        conflicts_seen: consecutive_conflicts,
    })
}

fn extract_conflict_list(message: &str) -> Vec<String> {
    let Some(idx) = message.find("conflicts: ") else {
        return Vec::new();
    };
    let tail = &message[idx + "conflicts: ".len()..];
    tail.split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect()
}

/// Outcome of [`apply_patch_with_structural_fallback`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum StructuralPatchOutcome {
    /// Patch applied as-is at the expected line range.
    AppliedExact,
    /// Patch applied after shifting to a structural-match offset.
    AppliedStructural { matched_offset: usize },
    /// Neither anchor matched.
    NoMatch,
}

/// Applies a `expected_lines` -> `replacement_lines` patch with a
/// fallback to a 3-line surrounding-context structural match when the
/// exact expected window isn't present (Runner-up #90 apply-patch
/// fallback). Returns the new text without writing it; callers decide
/// whether to atomically write.
pub(crate) fn apply_patch_with_structural_fallback(
    source: &str,
    expected_lines: &[&str],
    replacement_lines: &[&str],
    context_before: &[&str],
    context_after: &[&str],
) -> (String, StructuralPatchOutcome) {
    let lines: Vec<&str> = source.lines().collect();
    let exact = find_exact_window(&lines, expected_lines);
    if let Some(idx) = exact {
        let updated = splice_window(&lines, idx, expected_lines.len(), replacement_lines);
        return (updated, StructuralPatchOutcome::AppliedExact);
    }
    // Structural fallback: match the context-before window, then the
    // context-after window, and overwrite whatever sits between them.
    let strip = |s: &str| -> String {
        let trimmed = s.trim_start();
        // strip leading "<lineno>:" or "<lineno>\t" if present
        let rest = trimmed.find(|c: char| !c.is_ascii_digit()).map_or(trimmed, |i| {
            let (digits, rest) = trimmed.split_at(i);
            if digits.is_empty() {
                trimmed
            } else {
                rest.trim_start_matches([':', '\t', ' '])
            }
        });
        rest.trim_end().to_string()
    };
    let stripped_before: Vec<String> = context_before.iter().map(|s| strip(s)).collect();
    let stripped_after: Vec<String> = context_after.iter().map(|s| strip(s)).collect();
    let normalised: Vec<String> = lines.iter().map(|s| strip(s)).collect();
    let before_idx = find_exact_window(&normalised, &stripped_before);
    let after_idx = find_exact_window(&normalised, &stripped_after);
    match (before_idx, after_idx) {
        (Some(b), Some(a)) if a >= b + stripped_before.len() => {
            let splice_start = b + stripped_before.len();
            let splice_len = a - splice_start;
            let updated = splice_window(&lines, splice_start, splice_len, replacement_lines);
            (
                updated,
                StructuralPatchOutcome::AppliedStructural {
                    matched_offset: splice_start,
                },
            )
        }
        _ => (source.to_string(), StructuralPatchOutcome::NoMatch),
    }
}

fn find_exact_window<S: AsRef<str>>(haystack: &[S], needle: &[impl AsRef<str>]) -> Option<usize> {
    if needle.is_empty() || needle.len() > haystack.len() {
        return None;
    }
    'outer: for start in 0..=haystack.len() - needle.len() {
        for (i, item) in needle.iter().enumerate() {
            if haystack[start + i].as_ref() != item.as_ref() {
                continue 'outer;
            }
        }
        return Some(start);
    }
    None
}

fn splice_window(lines: &[&str], start: usize, len: usize, replacement: &[&str]) -> String {
    let mut out: Vec<&str> = Vec::with_capacity(lines.len() - len + replacement.len());
    out.extend_from_slice(&lines[..start]);
    out.extend_from_slice(replacement);
    out.extend_from_slice(&lines[start + len..]);
    out.join("\n")
}

/// Set of task IDs that would be demoted by a candidate plan rewrite.
///
/// Returned by [`detect_plan_demotions`] so the orchestrator can route
/// the conflict to a broker instead of silently losing receipts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PlanDemotionReport {
    pub(crate) demoted_task_ids: Vec<String>,
}

impl PlanDemotionReport {
    pub(crate) fn is_empty(&self) -> bool {
        self.demoted_task_ids.is_empty()
    }
}

/// Detects `[x] -> [ ]` or `[x] -> [~]` rewrites between two plan
/// snapshots. Each demoted task ID is returned so callers can refuse the
/// commit and surface the conflict.
pub(crate) fn detect_plan_demotions(previous: &str, next: &str) -> PlanDemotionReport {
    let prev_done = collect_done_task_ids(previous);
    let next_done = collect_done_task_ids(next);
    let mut demoted: Vec<String> = prev_done
        .into_iter()
        .filter(|id| !next_done.contains(id))
        .collect();
    demoted.sort();
    demoted.dedup();
    PlanDemotionReport {
        demoted_task_ids: demoted,
    }
}

fn collect_done_task_ids(plan: &str) -> std::collections::BTreeSet<String> {
    let mut out = std::collections::BTreeSet::new();
    for line in plan.lines() {
        let trimmed = line.trim_start();
        let rest = if let Some(rest) = trimmed.strip_prefix("- [x] ") {
            rest
        } else if let Some(rest) = trimmed.strip_prefix("- [X] ") {
            rest
        } else {
            continue;
        };
        if let Some(id) = extract_task_id(rest) {
            out.insert(id);
        }
    }
    out
}

fn extract_task_id(rest: &str) -> Option<String> {
    let trimmed = rest.trim_start();
    if let Some(after_tick) = trimmed.strip_prefix('`') {
        if let Some(end) = after_tick.find('`') {
            let id = after_tick[..end].trim();
            if !id.is_empty() {
                return Some(id.to_string());
            }
        }
    }
    let token: String = trimmed
        .chars()
        .take_while(|c| !c.is_whitespace())
        .collect();
    if token.is_empty() {
        None
    } else {
        Some(token)
    }
}

/// Refuses an in-flight commit whose IMPLEMENTATION_PLAN.md diff demotes
/// any `[x]` row to `[ ]`/`[~]`. Returns Ok when no demotion is staged.
///
/// `parent` is the parent commit to diff `IMPLEMENTATION_PLAN.md` against.
pub(crate) fn assert_no_plan_demotion(repo: &Path, parent: &str) -> Result<()> {
    let plan_relative = Path::new("IMPLEMENTATION_PLAN.md");
    let current_full = repo.join(plan_relative);
    if !current_full.exists() {
        return Ok(());
    }
    let current = fs::read_to_string(&current_full)
        .with_context(|| format!("failed to read {}", current_full.display()))?;
    let previous = git_show_path(repo, parent, "IMPLEMENTATION_PLAN.md").unwrap_or_default();
    let report = detect_plan_demotions(&previous, &current);
    if report.is_empty() {
        return Ok(());
    }
    bail!(
        "plan-integrity guard refused commit in {}: IMPLEMENTATION_PLAN.md demotes completed task(s): {}",
        repo.display(),
        report.demoted_task_ids.join(", ")
    );
}

fn git_show_path(repo: &Path, commit: &str, path: &str) -> Result<String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(["show", &format!("{commit}:{path}")])
        .output()
        .with_context(|| format!("failed to launch git in {}", repo.display()))?;
    if !output.status.success() {
        return Ok(String::new());
    }
    Ok(String::from_utf8_lossy(&output.stdout).to_string())
}

/// Computes a receipts anchor for the lane-owned paths and amends the
/// current HEAD commit with the anchor footer. Intended to run in the
/// SAME commit-cycle as the lane's auto-commit so queue-sync can verify
/// against a fresh anchor instead of chasing drift.
pub(crate) fn receipts_rehash_amend(repo: &Path, owned_paths: &[PathBuf]) -> Result<()> {
    if owned_paths.is_empty() {
        return Ok(());
    }
    let anchor = receipts::compute_anchor(repo, owned_paths)?;
    let footer = receipts::render_footer(&anchor);
    let head_message_output = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(["log", "-1", "--format=%B"])
        .output()
        .with_context(|| format!("failed to launch git in {}", repo.display()))?;
    if !head_message_output.status.success() {
        bail!(
            "receipts rehash: cannot read HEAD message in {}",
            repo.display()
        );
    }
    let head_message = String::from_utf8_lossy(&head_message_output.stdout)
        .trim_end()
        .to_string();
    if head_message.contains(receipts::RECEIPT_ANCHOR_COMMIT_KEY)
        && head_message.contains(receipts::RECEIPT_ANCHOR_CONTENT_KEY)
        && head_message.contains(&anchor.content_sha256)
    {
        return Ok(());
    }
    let trailer = if head_message.ends_with('\n') {
        format!("{head_message}\n{footer}")
    } else {
        format!("{head_message}\n\n{footer}")
    };
    let amend = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(["commit", "--amend", "--no-edit", "-m", &trailer])
        .output()
        .with_context(|| format!("failed to launch git in {}", repo.display()))?;
    if !amend.status.success() {
        bail!(
            "receipts rehash: git commit --amend failed in {}: {}",
            repo.display(),
            String::from_utf8_lossy(&amend.stderr).trim()
        );
    }
    Ok(())
}

fn scrub_parallel_receipt_staging(repo_root: &Path) -> Result<()> {
    let receipt_dir = repo_root.join(".auto/symphony/verification-receipts");
    if !receipt_dir.exists() {
        return Ok(());
    }
    let _ = run_git(
        repo_root,
        [
            "restore",
            "--staged",
            "--worktree",
            "--",
            ".auto/symphony/verification-receipts",
        ],
    );
    let _ = run_git(
        repo_root,
        ["clean", "-fd", "--", ".auto/symphony/verification-receipts"],
    );
    Ok(())
}

fn landing_error_suggests_dirty_canonical_worktree(err: &anyhow::Error) -> bool {
    let message = format!("{err:#}").to_ascii_lowercase();
    message.contains("would be overwritten by merge")
        || message.contains("please commit your changes or stash them")
        || message.contains("untracked working tree files would be overwritten")
}

fn try_auto_checkpoint_canonical_for_landing(
    repo_root: &Path,
    target_branch: &str,
    assignment: &ActiveLaneAssignment,
    reason: &str,
) -> Result<bool> {
    let Some(commit) =
        auto_checkpoint_if_needed(repo_root, target_branch, "auto parallel checkpoint")?
    else {
        return Ok(false);
    };
    println!(
        "checkpoint:  committed canonical changes at {commit} {reason} for lane-{} `{}`",
        assignment.lane_index, assignment.task.id
    );
    append_lane_host_event(
        &assignment.stdout_log_path,
        assignment.lane_index,
        &assignment.task.id,
        &format!("checkpoint: committed canonical changes at {commit} {reason}"),
    );
    Ok(true)
}

fn update_task_completion_in_plan(
    repo_root: &Path,
    task_id: &str,
    status: LoopTaskStatus,
) -> Result<bool> {
    let plan_path = repo_root.join("IMPLEMENTATION_PLAN.md");
    if !plan_path.exists() {
        return Ok(false);
    }

    let plan = fs::read_to_string(&plan_path)
        .with_context(|| format!("failed to read {}", plan_path.display()))?;
    let updated = update_task_completion_in_plan_text(&plan, task_id, status);
    if updated == plan {
        return Ok(false);
    }

    atomic_write(&plan_path, updated.as_bytes())
        .with_context(|| format!("failed to write {}", plan_path.display()))?;
    Ok(true)
}

fn update_task_completion_in_plan_text(
    plan: &str,
    task_id: &str,
    status: LoopTaskStatus,
) -> String {
    let mut updated = String::new();

    for chunk in plan.split_inclusive('\n') {
        let line = chunk.trim_end_matches('\n').trim_end_matches('\r');
        if let Some((_, current_task_id, _)) = parse_task_header(line) {
            if current_task_id == task_id {
                updated.push_str(&mark_task_header_status(chunk, status));
                continue;
            }
        }
        updated.push_str(chunk);
    }

    updated
}

fn mark_task_header_status(line: &str, status: LoopTaskStatus) -> String {
    let newline = if line.ends_with("\r\n") {
        "\r\n"
    } else if line.ends_with('\n') {
        "\n"
    } else {
        ""
    };
    let stripped = line.trim_end_matches('\n').trim_end_matches('\r');
    let indent_len = stripped.len() - stripped.trim_start().len();
    let indent = &stripped[..indent_len];
    let trimmed = stripped.trim_start();
    let (existing_done, rest) = if let Some(rest) = trimmed.strip_prefix("- [x] ") {
        (true, rest)
    } else if let Some(rest) = trimmed.strip_prefix("- [X] ") {
        (true, rest)
    } else if let Some(rest) = trimmed.strip_prefix("- [ ] ") {
        (false, rest)
    } else if let Some(rest) = trimmed.strip_prefix("- [!] ") {
        (false, rest)
    } else if let Some(rest) = trimmed.strip_prefix("- [~] ") {
        (false, rest)
    } else {
        (false, trimmed)
    };
    // Completion is monotonic forward: once a task header is marked [x], an
    // automated reconcile pass must not demote it. This guards against
    // duplicate-ID rows in IMPLEMENTATION_PLAN.md where landing one row would
    // otherwise rewrite a sibling [x] row that shares the same task ID.
    if existing_done && status != LoopTaskStatus::Done {
        return line.to_string();
    }
    let marker = match status {
        LoopTaskStatus::Pending => "- [ ]",
        LoopTaskStatus::Blocked => "- [!]",
        LoopTaskStatus::Partial => "- [~]",
        LoopTaskStatus::Done => "- [x]",
    };
    format!("{indent}{marker} {rest}{newline}")
}

