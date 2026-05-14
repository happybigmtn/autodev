    use std::collections::{BTreeMap, BTreeSet};
    use std::fs;
    use std::path::PathBuf;
    use std::process::Command;
    use std::time::{SystemTime, UNIX_EPOCH};

    use anyhow::anyhow;

    use crate::task_parser::LaneKind;
    use crate::{ParallelAction, ParallelArgs, ParallelCargoTarget};

    use super::{
        audit_parallel_completion_drift, build_iteration_prompt, build_parallel_lane_prompt,
        checkpoint_parallel_host_queue_changes, cherry_pick_lane_range,
        classify_parallel_preflight_needs, clear_partial_follow_up_tracking,
        default_cargo_build_jobs_for, dirty_worktree_recovery_note, discover_sibling_git_repos,
        effective_parallel_claude_max_turns, environment_blocker_reason,
        host_queue_state_files_for_repo, inspect_lane_repo_progress, is_linear_usage_limit_error,
        is_verification_only_task, landing_error_suggests_dirty_canonical_worktree,
        landing_recovery_note, lane_repo_has_active_cherry_pick, lane_repo_has_rebase_recovery,
        lane_repo_recovery_note, lane_repo_status_summary, lane_scope_budget, lane_status_task_id,
        last_parallel_stop_state, maybe_disable_linear_auto_sync_for_run,
        next_parallel_unblock_candidate, no_dependency_ready_stop_message,
        parallel_blocker_frontier, parallel_run_root, parallel_status_safety_verdict,
        parallel_tmux_command, parallel_tmux_session_name, parse_lane_repo_process_pids,
        parse_loop_plan, parse_parallel_stop_ids, preflight_warning_names,
        prepare_lane_landing_recovery, prepare_parallel_startup, prepared_landing_recovery_note,
        preserve_resume_recovery_notes, prioritize_ready_parallel_tasks, read_lane_task_id,
        ready_parallel_tasks, receipt_drift_status_summary, recent_parallel_host_warnings,
        record_partial_follow_up, render_default_parallel_prompt, render_parallel_health_summary,
        repair_parallel_canonical_before_dispatch, repo_forbids_legacy_review_trackers,
        reset_parallel_lane_root, resolve_loop_worker_env, resolve_reference_repos,
        retire_superseded_lane_cherry_pick_recovery, salvage_recovery_note,
        superseded_lane_cherry_pick_recovery, take_resume_candidate_for_task,
        task_id_from_prompt_filename, tmux_status_line_has_live_worker,
        try_checkpoint_parallel_host_queue_changes, update_task_completion_in_plan_text,
        validate_lane_assignment_metadata, write_lane_assignment_metadata,
        write_operator_actions_for_ready_tasks, ActiveLaneAssignment, CherryPickFailurePolicy,
        LaneLandingRecoveryPrep, LaneRepoProgress, LaneResumeCandidate, LinearAutoSyncState,
        LoopQueueSnapshot, LoopTask, LoopTaskStatus, ParallelBlockerKind, ParallelEventLogger,
        ParallelPreflightNeeds, ParallelStartupPrep, ParallelUnblockCandidateKind,
        PartialFollowUpDisposition,
    };

    #[test]
    fn default_prompt_uses_resolved_branch() {
        let prompt = render_default_parallel_prompt("trunk", &[]);
        assert!(prompt.contains("branch `trunk`"));
        assert!(!prompt.contains("origin/main"));
        assert!(prompt.contains("Study `AGENTS.md` for repo-specific build"));
        assert!(prompt.contains("RED/GREEN/REFACTOR"));
        assert!(prompt.contains("failing test"));
        assert!(prompt
            .contains("identify the first actionable unfinished task marked `- [ ]` or `- [~]`"));
        assert!(prompt.contains("historical context only"));
        assert!(prompt.contains("first actionable unfinished `- [ ]` or `- [~]` task"));
        assert!(prompt.contains("Completion path: <TASK-ID>"));
        assert!(prompt.contains("mark it `- [x]` only when local verification, review handoff, and required completion artifacts are actually in place"));
    }

    #[test]
    fn default_prompt_lists_reference_repos_when_declared() {
        let prompt =
            render_default_parallel_prompt("main", &[PathBuf::from("/tmp/robopokermulti")]);
        assert!(prompt.contains("Additional repositories you may inspect as read-only context"));
        assert!(prompt.contains("/tmp/robopokermulti"));
        assert!(prompt.contains("Do not edit, format, stage, commit, push"));
        assert!(prompt.contains("leave a precise follow-up plan item or blocker"));
    }

    #[test]
    fn parallel_tmux_session_name_uses_repo_slug() {
        assert_eq!(
            parallel_tmux_session_name(&PathBuf::from("/home/r/Coding/bitino")),
            "bitino-parallel"
        );
        assert_eq!(
            parallel_tmux_session_name(&PathBuf::from("/tmp/weird:repo name")),
            "weird-repo-name-parallel"
        );
    }

    #[test]
    fn parallel_tmux_command_persists_host_logs_and_keeps_shell_open() {
        let args = ParallelArgs {
            action: None,
            max_iterations: Some(3),
            max_concurrent_workers: 8,
            cargo_build_jobs: Some(2),
            cargo_target: ParallelCargoTarget::Lane,
            prompt_file: Some(PathBuf::from("/tmp/prompt.md")),
            model: "gpt-5.5".to_string(),
            reasoning_effort: "high".to_string(),
            branch: Some("main".to_string()),
            reference_repos: vec![PathBuf::from("/tmp/reference repo")],
            include_siblings: true,
            run_root: Some(PathBuf::from("/tmp/auto-parallel")),
            codex_bin: PathBuf::from("codex"),
            claude: false,
            max_turns: None,
            max_retries: 2,
        };
        let command = parallel_tmux_command(&PathBuf::from("/tmp/auto-parallel"), &args)
            .expect("tmux command should render");

        assert!(command.contains("host.stdout.log"));
        assert!(command.contains("host.stderr.log"));
        assert!(command.contains("tee -a"));
        assert!(command.contains("exec bash"));
        assert!(command.contains(" parallel "));
        assert!(command.contains("--threads 8"));
        assert!(command.contains("--max-iterations 3"));
        assert!(command.contains("--cargo-target lane"));
        assert!(command.contains("--reference-repo"));
        assert!(command.contains("--include-siblings"));
        assert!(!command.contains(" super "));
    }

    #[test]
    fn parallel_tmux_command_renders_status_action_when_requested() {
        let args = ParallelArgs {
            action: Some(ParallelAction::Status),
            max_iterations: None,
            max_concurrent_workers: 2,
            cargo_build_jobs: None,
            cargo_target: ParallelCargoTarget::Auto,
            prompt_file: None,
            model: "gpt-5.5".to_string(),
            reasoning_effort: "high".to_string(),
            branch: None,
            reference_repos: Vec::new(),
            include_siblings: false,
            run_root: None,
            codex_bin: PathBuf::from("codex"),
            claude: false,
            max_turns: None,
            max_retries: 2,
        };
        let command = parallel_tmux_command(&PathBuf::from("/tmp/auto-parallel"), &args)
            .expect("tmux command should render");

        assert!(command.contains(" parallel "));
        assert!(command.contains(" status"));
    }

    #[test]
    fn tmux_status_worker_detection_ignores_parked_shells() {
        assert!(tmux_status_line_has_live_worker("0:host:dead=0:cmd=auto"));
        assert!(tmux_status_line_has_live_worker(
            "1:lane-1:dead=0:cmd=codex"
        ));
        assert!(!tmux_status_line_has_live_worker("0:host:dead=0:cmd=bash"));
        assert!(!tmux_status_line_has_live_worker(
            "1:lane-1:dead=1:cmd=auto"
        ));
    }

    #[test]
    fn parallel_run_root_resolves_relative_override_under_repo_root() {
        let args = ParallelArgs {
            action: None,
            max_iterations: None,
            max_concurrent_workers: 5,
            cargo_build_jobs: None,
            cargo_target: ParallelCargoTarget::Auto,
            prompt_file: None,
            model: "gpt-5.5".to_string(),
            reasoning_effort: "high".to_string(),
            branch: None,
            reference_repos: Vec::new(),
            include_siblings: false,
            run_root: Some(PathBuf::from(".auto/super/run-1")),
            codex_bin: PathBuf::from("codex"),
            claude: false,
            max_turns: None,
            max_retries: 2,
        };

        assert_eq!(
            parallel_run_root(&PathBuf::from("/repo"), &args),
            PathBuf::from("/repo/.auto/super/run-1")
        );
    }

    #[test]
    fn parallel_startup_prep_checkpoints_dirty_worktree_before_bootstrap() {
        let (root, _remote, _upstream, worker) =
            init_remote_and_clones("parallel-startup-prep", "trunk");

        fs::create_dir_all(worker.join("notes")).expect("failed to create notes dir");
        fs::write(worker.join("notes").join("draft.md"), "draft\n").expect("failed to write draft");

        let prep =
            prepare_parallel_startup(&worker, "trunk").expect("parallel startup prep should work");
        let commit = match prep {
            ParallelStartupPrep::Checkpointed(commit) => commit,
            other => panic!("expected checkpointed startup prep, got {other:?}"),
        };

        assert!(!commit.is_empty());
        assert_eq!(run_git_in(&worker, ["status", "--short"]), "");
        assert!(worker.join("notes").join("draft.md").exists());
        let log = run_git_in(&worker, ["log", "--format=%s", "-2"]);
        assert_eq!(log, "worker: auto parallel checkpoint\ninit\n");

        fs::remove_dir_all(&root).expect("failed to remove temp repo");
    }

    #[test]
    fn host_queue_sync_failures_are_logged_without_aborting() {
        let run_root = unique_temp_dir("parallel-host-queue-warning");
        let repo_root = unique_temp_dir("parallel-host-queue-warning-repo");
        fs::create_dir_all(&run_root).expect("failed to create run root");
        fs::create_dir_all(&repo_root).expect("failed to create repo root");
        fs::write(repo_root.join("IMPLEMENTATION_PLAN.md"), "# plan\n")
            .expect("failed to write queue file");

        let logger = ParallelEventLogger::new(&run_root).expect("parallel logger should init");
        try_checkpoint_parallel_host_queue_changes(&repo_root, "main", &logger);

        let live_log =
            fs::read_to_string(run_root.join("live.log")).expect("live log should be readable");
        assert!(live_log.contains("failed syncing host-owned queue state"));
        assert!(live_log.contains("continuing without a host queue commit"));

        fs::remove_dir_all(&run_root).expect("failed to remove run root");
        fs::remove_dir_all(&repo_root).expect("failed to remove repo root");
    }

    #[test]
    fn lane_prompt_requires_clean_committed_finish_and_can_include_recovery_context() {
        let snapshot = parse_loop_plan(
            r#"- [ ] `TASK-001` First task
  Dependencies:
  - None
  Estimated scope: small
"#,
        );
        let task = snapshot.tasks.first().expect("task should parse");
        let prompt = build_parallel_lane_prompt(
            "base prompt",
            &snapshot,
            task,
            "trunk",
            "Use the host-provided `CARGO_TARGET_DIR`; this run gives each lane its own target directory.",
            "- warn agent-browser: daemon missing",
            Some("Resolve the previous landing conflict."),
        );

        assert!(prompt.contains("run `git status --short`"));
        assert!(prompt.contains("at least one local commit for this task and a clean worktree"));
        assert!(prompt.contains("reports `0 tests`"));
        assert!(prompt.contains("direct target-dir test binaries"));
        assert!(prompt.contains("AUTO_ENV_BLOCKER"));
        assert!(prompt.contains("Host-parsed executable verification commands"));
        assert!(
            prompt.contains("Do not treat narrative `Verification:` prose as literal shell input")
        );
        assert!(prompt.contains("Host preflight report:"));
        assert!(prompt.contains("Host recovery context:"));
        assert!(prompt.contains("Resolve the previous landing conflict."));
    }

    #[test]
    fn lane_status_task_id_reports_idle_when_latest_log_is_idle() {
        assert_eq!(
            lane_status_task_id(
                "OLD-TASK",
                false,
                Some("[auto parallel host lane-5 [idle]] idle: waiting on dependencies"),
            ),
            "[idle]"
        );
        assert_eq!(
            lane_status_task_id("OLD-TASK", true, Some("anything")),
            "OLD-TASK"
        );
    }

    #[test]
    fn recovery_notes_explain_semantic_merge_and_dirty_cleanup_contracts() {
        let landing = landing_recovery_note("trunk", "conflict in src/lib.rs");
        assert!(landing.contains("Resolve conflicts semantically"));
        assert!(landing.contains("GIT_EDITOR=true git rebase --continue"));
        assert!(landing.contains("based on the latest `trunk`"));
        assert!(landing.contains("conflict in src/lib.rs"));

        let prepared = prepared_landing_recovery_note(
            "trunk",
            "git cherry-pick failed",
            "git cherry-pick stopped at src/lib.rs",
        );
        assert!(prepared.contains("landing-recovery mode"));
        assert!(prepared.contains("git cherry-pick"));
        assert!(prepared.contains("cherry-pick --continue"));
        assert!(prepared.contains("git cherry-pick stopped at src/lib.rs"));

        let dirty = dirty_worktree_recovery_note("M src/lib.rs");
        assert!(dirty.contains("Run `git status --short`"));
        assert!(dirty.contains("include it in a local task commit"));
        assert!(dirty.contains("unrelated formatter spillover"));
        assert!(dirty.contains("revert just that file"));
        assert!(dirty.contains("M src/lib.rs"));
    }

    #[test]
    fn stale_rebase_merge_state_is_reported_with_cleanup_recipe() {
        let repo = unique_temp_dir("parallel-stale-rebase-merge");
        fs::create_dir_all(&repo).expect("failed to create temp repo");
        run_git_in(&repo, ["init", "-b", "main"]);
        run_git_in(&repo, ["config", "user.name", "autodev tests"]);
        run_git_in(&repo, ["config", "user.email", "autodev@example.com"]);
        fs::write(repo.join("README.md"), "init\n").expect("failed to write readme");
        run_git_in(&repo, ["add", "README.md"]);
        run_git_in(&repo, ["commit", "-m", "init"]);

        let rebase_merge = repo.join(".git").join("rebase-merge");
        fs::create_dir_all(&rebase_merge).expect("failed to create stale rebase dir");
        fs::write(rebase_merge.join("autostash"), "deadbeef\n")
            .expect("failed to write stale autostash");

        assert!(lane_repo_has_rebase_recovery(&repo));
        let summary = lane_repo_status_summary(&repo);
        assert!(summary.contains("stale rebase-merge"));
        let note = lane_repo_recovery_note(&repo, "main", " M README.md");
        assert!(note.contains("git rebase --abort"));
        assert!(note.contains("rebase-merge"));
        assert!(note.contains("autostash"));

        fs::remove_dir_all(&repo).expect("failed to remove temp repo");
    }

    #[test]
    fn environment_blocker_detection_prefers_explicit_marker() {
        let log = "some output\nAUTO_ENV_BLOCKER: regtest RPC is down\nmore output";
        assert_eq!(
            environment_blocker_reason(log),
            Some("regtest RPC is down".to_string())
        );

        assert_eq!(
            environment_blocker_reason(
                "Daemon failed to start (socket: /run/user/1000/agent-browser/default.sock)"
            ),
            Some("agent-browser daemon failed to start".to_string())
        );
    }

    #[test]
    fn detects_direct_review_queue_policy() {
        let temp = unique_temp_dir("loop-direct-review-policy");
        fs::create_dir_all(&temp).expect("failed to create temp dir");
        fs::write(
            temp.join("WORKFLOW.md"),
            "Do not restore `COMPLETED.md`, `WORKLIST.md`, or `ARCHIVED.md`; use `REVIEW.md`.",
        )
        .expect("failed to write policy");

        assert!(repo_forbids_legacy_review_trackers(&temp));

        fs::remove_dir_all(&temp).expect("failed to remove temp dir");
    }

    #[test]
    fn default_cargo_build_jobs_caps_nested_parallelism() {
        assert_eq!(default_cargo_build_jobs_for(22, 1), 4);
        assert_eq!(default_cargo_build_jobs_for(22, 5), 3);
        assert_eq!(default_cargo_build_jobs_for(12, 4), 2);
        assert_eq!(default_cargo_build_jobs_for(3, 2), 1);
        assert_eq!(default_cargo_build_jobs_for(1, 1), 1);
    }

    #[test]
    fn no_dependency_ready_stop_message_calls_out_shelved_tasks() {
        let plan = parse_loop_plan(
            r#"
- [ ] `TASK-1` First
  Dependencies: `TASK-3`
- [ ] `TASK-2` Second
  Dependencies: `TASK-4`
- [ ] `TASK-3` Blocker one
  Dependencies: none
- [ ] `TASK-4` Blocker two
  Dependencies: none
"#,
        );
        let queue = LoopQueueSnapshot {
            pending_ids: vec!["TASK-1".to_string(), "TASK-2".to_string()],
            blocked_ids: vec!["TASK-9".to_string()],
        };
        let mut shelved = BTreeMap::new();
        shelved.insert("TASK-3".to_string(), "- [ ] `TASK-3`".to_string());
        shelved.insert("TASK-4".to_string(), "- [ ] `TASK-4`".to_string());
        let deferred = BTreeSet::from(["TASK-5".to_string()]);

        let attempts = BTreeMap::from([("TASK-5".to_string(), 4usize)]);
        let message = no_dependency_ready_stop_message(
            &plan,
            &BTreeSet::new(),
            &queue,
            &shelved,
            &deferred,
            &attempts,
            4,
        );
        assert!(message.contains("stopping with unresolved shelved tasks"));
        assert!(message.contains("pending: TASK-1, TASK-2"));
        assert!(message.contains("blocked: TASK-9"));
        assert!(message.contains("shelved: TASK-3, TASK-4"));
        assert!(message.contains("deferred: TASK-5"));
        assert!(message.contains("exhausted-unblock-attempts: TASK-5=4/4"));
        assert!(message.contains("frontier: TASK-3 [shelved]"));
    }

    #[test]
    fn parallel_blocker_frontier_classifies_shelved_and_deferred_dependencies() {
        let plan = parse_loop_plan(
            r#"
- [ ] `TASK-A` waits on shelved
  Dependencies: `TASK-S`
- [ ] `TASK-B` waits on deferred
  Dependencies: `TASK-P`
- [ ] `TASK-S` shelved blocker
  Dependencies: none
- [~] `TASK-P` partial blocker
  Dependencies: none
"#,
        );
        let shelved = BTreeMap::from([(
            "TASK-S".to_string(),
            "- [ ] `TASK-S` shelved blocker".to_string(),
        )]);
        let deferred = BTreeSet::from(["TASK-P".to_string()]);

        let frontier = parallel_blocker_frontier(&plan, &BTreeSet::new(), &shelved, &deferred);
        assert_eq!(frontier[0].task_id, "TASK-P");
        assert_eq!(frontier[0].kind, ParallelBlockerKind::DeferredPartial);
        assert_eq!(frontier[1].task_id, "TASK-S");
        assert_eq!(frontier[1].kind, ParallelBlockerKind::Shelved);
    }

    #[test]
    fn next_parallel_unblock_candidate_prefers_resumable_shelved_blocker() {
        let plan = parse_loop_plan(
            r#"
- [ ] `TASK-A` blocked by shelved
  Dependencies: `TASK-S`
- [ ] `TASK-B` blocked by shelved
  Dependencies: `TASK-S`
- [ ] `TASK-C` blocked by deferred
  Dependencies: `TASK-P`
- [ ] `TASK-S` ready shelved blocker
  Dependencies: none
- [~] `TASK-P` ready deferred blocker
  Dependencies: none
"#,
        );
        let task_s = plan.task("TASK-S").expect("TASK-S should exist").clone();
        let shelved = BTreeMap::from([("TASK-S".to_string(), task_s.markdown.clone())]);
        let deferred = BTreeSet::from(["TASK-P".to_string()]);
        let resumable = BTreeMap::from([(
            2usize,
            LaneResumeCandidate {
                lane_index: 2,
                task: task_s,
                lane_root: PathBuf::from("/tmp/lane-2"),
                lane_repo_root: PathBuf::from("/tmp/lane-2/repo"),
                base_commit: "abc123".to_string(),
                stdout_log_path: PathBuf::from("/tmp/lane-2/stdout.log"),
                stderr_log_path: PathBuf::from("/tmp/lane-2/stderr.log"),
                worker_pid_path: PathBuf::from("/tmp/lane-2/worker.pid"),
                host_recovery_note: Some("recover".to_string()),
            },
        )]);

        let candidate = next_parallel_unblock_candidate(
            &plan,
            &BTreeSet::new(),
            &shelved,
            &deferred,
            &resumable,
            &BTreeMap::new(),
            4,
        )
        .expect("expected an unblock candidate");
        assert_eq!(candidate.task.id, "TASK-S");
        assert_eq!(candidate.kind, ParallelUnblockCandidateKind::ShelvedResume);
    }

    #[test]
    fn next_parallel_unblock_candidate_retries_until_attempt_limit() {
        let plan = parse_loop_plan(
            r#"
- [ ] `TASK-A` blocked by partial
  Dependencies: `TASK-P`
- [~] `TASK-P` partial blocker
  Dependencies: none
"#,
        );
        let deferred = BTreeSet::from(["TASK-P".to_string()]);
        let mut attempts = BTreeMap::from([("TASK-P".to_string(), 3usize)]);

        let candidate = next_parallel_unblock_candidate(
            &plan,
            &BTreeSet::new(),
            &BTreeMap::new(),
            &deferred,
            &BTreeMap::new(),
            &attempts,
            4,
        )
        .expect("attempt 4 should still be eligible");
        assert_eq!(candidate.task.id, "TASK-P");
        assert_eq!(
            candidate.kind,
            ParallelUnblockCandidateKind::DeferredPartialCloseout
        );

        attempts.insert("TASK-P".to_string(), 4);
        assert!(next_parallel_unblock_candidate(
            &plan,
            &BTreeSet::new(),
            &BTreeMap::new(),
            &deferred,
            &BTreeMap::new(),
            &attempts,
            4,
        )
        .is_none());
    }

    #[test]
    fn parse_parallel_stop_ids_extracts_fields() {
        let line = "no dependency-ready tasks remain to dispatch; stopping with unresolved shelved tasks. pending: A, B blocked: none shelved: C, D deferred: E frontier: C [shelved] -> A, B";
        assert_eq!(
            parse_parallel_stop_ids(line, "shelved:"),
            BTreeSet::from(["C".to_string(), "D".to_string()])
        );
        assert_eq!(
            parse_parallel_stop_ids(line, "deferred:"),
            BTreeSet::from(["E".to_string()])
        );
    }

    #[test]
    fn last_parallel_stop_state_reads_latest_stop_line() {
        let run_root = unique_temp_dir("parallel-stop-state");
        fs::create_dir_all(&run_root).expect("failed to create run root");
        fs::write(
            run_root.join("live.log"),
            "idle: something\nno dependency-ready tasks remain to dispatch; stopping with unresolved shelved tasks. pending: A blocked: none shelved: C, D deferred: E frontier: C [shelved] -> A\n",
        )
        .expect("failed to write live log");
        let state = last_parallel_stop_state(&run_root).expect("expected stop state");
        assert_eq!(
            state.shelved,
            BTreeSet::from(["C".to_string(), "D".to_string()])
        );
        assert_eq!(state.deferred, BTreeSet::from(["E".to_string()]));
        fs::remove_dir_all(&run_root).expect("failed to remove run root");
    }

    #[test]
    fn salvage_recovery_note_reuses_saved_landing_error() {
        let run_root = unique_temp_dir("parallel-salvage-note");
        let lane_root = run_root.join("lanes").join("lane-3");
        fs::create_dir_all(&lane_root).expect("failed to create lane root");
        fs::create_dir_all(run_root.join("salvage")).expect("failed to create salvage dir");
        fs::write(
            run_root.join("salvage").join("lane-3-TASK-1.md"),
            "# auto parallel salvage\n\n## Landing Error\n\n```text\ngit cherry-pick failed in /tmp/repo: conflict\n```\n\n## Recovery\n\nReconcile it.\n",
        )
        .expect("failed to write salvage note");

        let note = salvage_recovery_note(&lane_root, 3, "TASK-1", "main").expect("expected note");
        assert!(note.contains("git cherry-pick failed in /tmp/repo: conflict"));
        fs::remove_dir_all(&run_root).expect("failed to remove run root");
    }

    #[test]
    fn dirty_canonical_landing_errors_are_detected() {
        let err = anyhow!(
            "git cherry-pick failed in /tmp/repo: error: Your local changes to the following files would be overwritten by merge:\n  src/lib.rs\nPlease commit your changes or stash them before you merge.\nAborting\nfatal: cherry-pick failed"
        );
        assert!(landing_error_suggests_dirty_canonical_worktree(&err));
    }

    #[test]
    fn repair_parallel_canonical_checkpoints_dirty_dispatch_paths() {
        let (root, _remote, _upstream, worker) =
            init_remote_and_clones("parallel-repair-checkpoint", "trunk");
        let run_root = root.join("run");
        fs::create_dir_all(&run_root).expect("failed to create run root");
        let logger = ParallelEventLogger::new(&run_root).expect("logger should initialize");
        fs::write(worker.join("README.md"), "# dirty\n").expect("failed to dirty README");

        repair_parallel_canonical_before_dispatch(&worker, "trunk", &logger)
            .expect("dirty dispatch paths should be checkpointed");

        assert_eq!(run_git_in(&worker, ["status", "--short"]), "");
        let log = run_git_in(&worker, ["log", "--format=%s", "-1"]);
        assert_eq!(log.trim(), "worker: auto parallel checkpoint");
        fs::remove_dir_all(&root).expect("failed to remove temp root");
    }

    #[test]
    fn repair_parallel_canonical_checkpoints_verification_receipts() {
        let (root, _remote, _upstream, worker) =
            init_remote_and_clones("parallel-repair-receipts", "trunk");
        let run_root = root.join("run");
        fs::create_dir_all(&run_root).expect("failed to create run root");
        let logger = ParallelEventLogger::new(&run_root).expect("logger should initialize");
        let receipt_dir = worker.join(".auto/symphony/verification-receipts");
        fs::create_dir_all(&receipt_dir).expect("failed to create receipts dir");
        fs::write(receipt_dir.join("TASK-1.json"), "{\"status\":\"passed\"}\n")
            .expect("failed to write receipt");

        repair_parallel_canonical_before_dispatch(&worker, "trunk", &logger)
            .expect("dirty receipt should be checkpointed");

        assert_eq!(run_git_in(&worker, ["status", "--short"]), "");
        let log = run_git_in(&worker, ["log", "--format=%s", "-1"]);
        assert_eq!(log.trim(), "worker: auto parallel checkpoint");
        let committed = run_git_in(&worker, ["show", "--name-only", "--format=", "HEAD"]);
        assert!(committed.contains(".auto/symphony/verification-receipts/TASK-1.json"));
        fs::remove_dir_all(&root).expect("failed to remove temp root");
    }

    #[test]
    fn repair_parallel_canonical_removes_stale_zero_byte_index_lock() {
        let (root, _remote, _upstream, worker) =
            init_remote_and_clones("parallel-repair-stale-index-lock", "trunk");
        let run_root = root.join("run");
        fs::create_dir_all(&run_root).expect("failed to create run root");
        let logger = ParallelEventLogger::new(&run_root).expect("logger should initialize");
        let lock = worker.join(".git").join("index.lock");
        fs::write(&lock, "").expect("failed to write stale index lock");
        set_file_mtime_epoch(&lock);

        repair_parallel_canonical_before_dispatch(&worker, "trunk", &logger)
            .expect("stale zero-byte index lock should be repaired");

        assert!(!lock.exists(), "stale index lock should be removed");
        let live_log =
            fs::read_to_string(run_root.join("live.log")).expect("live log should be readable");
        assert!(live_log.contains("removed stale canonical git index lock"));
        fs::remove_dir_all(&root).expect("failed to remove temp root");
    }

    #[test]
    fn repair_parallel_canonical_refuses_fresh_zero_byte_index_lock() {
        let (root, _remote, _upstream, worker) =
            init_remote_and_clones("parallel-repair-fresh-index-lock", "trunk");
        let run_root = root.join("run");
        fs::create_dir_all(&run_root).expect("failed to create run root");
        let logger = ParallelEventLogger::new(&run_root).expect("logger should initialize");
        let lock = worker.join(".git").join("index.lock");
        fs::write(&lock, "").expect("failed to write fresh index lock");

        let err = repair_parallel_canonical_before_dispatch(&worker, "trunk", &logger)
            .expect_err("fresh index lock should require operator confirmation");

        assert!(lock.exists(), "fresh index lock should remain in place");
        let message = err.to_string();
        assert!(message.contains("active git index lock"));
        assert!(message.contains("context=before dispatch"));
        fs::remove_dir_all(&root).expect("failed to remove temp root");
    }

    #[test]
    fn repair_parallel_canonical_refuses_non_empty_stale_index_lock() {
        let (root, _remote, _upstream, worker) =
            init_remote_and_clones("parallel-repair-nonempty-index-lock", "trunk");
        let run_root = root.join("run");
        fs::create_dir_all(&run_root).expect("failed to create run root");
        let logger = ParallelEventLogger::new(&run_root).expect("logger should initialize");
        let lock = worker.join(".git").join("index.lock");
        fs::write(&lock, "git pid maybe alive\n").expect("failed to write non-empty index lock");
        set_file_mtime_epoch(&lock);

        let err = repair_parallel_canonical_before_dispatch(&worker, "trunk", &logger)
            .expect_err("non-empty index lock should not be auto-removed");

        assert!(lock.exists(), "non-empty index lock should remain in place");
        let message = err.to_string();
        assert!(message.contains("active git index lock"));
        assert!(message.contains("size=20"));
        fs::remove_dir_all(&root).expect("failed to remove temp root");
    }

    #[test]
    fn host_queue_checkpoint_removes_stale_zero_byte_index_lock_before_status() {
        let (root, _remote, _upstream, worker) =
            init_remote_and_clones("parallel-host-queue-stale-index-lock", "trunk");
        let run_root = root.join("run");
        fs::create_dir_all(&run_root).expect("failed to create run root");
        let logger = ParallelEventLogger::new(&run_root).expect("logger should initialize");
        fs::write(worker.join("IMPLEMENTATION_PLAN.md"), "# plan\n").expect("failed to write plan");
        run_git_in(&worker, ["add", "IMPLEMENTATION_PLAN.md"]);
        run_git_in(&worker, ["commit", "-m", "plan"]);
        run_git_in(&worker, ["push", "origin", "trunk"]);
        fs::write(
            worker.join("IMPLEMENTATION_PLAN.md"),
            "# plan\n\n- [x] done\n",
        )
        .expect("failed to dirty plan");
        let lock = worker.join(".git").join("index.lock");
        fs::write(&lock, "").expect("failed to write stale index lock");
        set_file_mtime_epoch(&lock);

        let commit = checkpoint_parallel_host_queue_changes(&worker, "trunk", &logger)
            .expect("stale index lock should be repaired before queue sync")
            .expect("queue sync should create a commit");

        assert!(!commit.is_empty());
        assert!(!lock.exists(), "stale index lock should be removed");
        assert_eq!(run_git_in(&worker, ["status", "--short"]), "");
        let log = run_git_in(&worker, ["log", "--format=%s", "-1"]);
        assert_eq!(log.trim(), "worker: parallel host queue sync");
        fs::remove_dir_all(&root).expect("failed to remove temp root");
    }

    #[test]
    fn host_queue_state_files_skip_missing_untracked_docs() {
        let repo = unique_temp_dir("parallel-host-queue-files");
        init_git_repo(&repo);
        fs::write(repo.join("IMPLEMENTATION_PLAN.md"), "# plan\n").expect("failed to write plan");
        fs::write(repo.join("COMPLETED.md"), "# completed\n").expect("failed to write completed");
        run_git_in(&repo, ["add", "IMPLEMENTATION_PLAN.md", "COMPLETED.md"]);
        run_git_in(&repo, ["commit", "-m", "queue docs"]);
        fs::remove_file(repo.join("COMPLETED.md")).expect("failed to remove completed");

        let files = host_queue_state_files_for_repo(&repo);
        assert!(files.contains(&"IMPLEMENTATION_PLAN.md"));
        assert!(files.contains(&"COMPLETED.md"));
        assert!(!files.contains(&"WORKLIST.md"));

        fs::remove_dir_all(&repo).expect("failed to remove temp repo");
    }

    #[test]
    fn parallel_health_summary_reports_preflight_host_and_recovery_issues() {
        let run_root = unique_temp_dir("parallel-health-summary");
        fs::create_dir_all(&run_root).expect("failed to create run root");
        fs::write(
            run_root.join("preflight.txt"),
            "- ok cargo: Rust workspace detected\n- warn agent-browser: missing\n- warn docker compose: missing\n",
        )
        .expect("failed to write preflight");
        fs::write(
            run_root.join("live.log"),
            "warning: failed syncing host-owned queue state\nwarning: failed syncing host-owned queue state\nwarning: lane-1 something else\n",
        )
        .expect("failed to write live log");

        let preflight = preflight_warning_names(&run_root);
        let host_warnings = recent_parallel_host_warnings(&run_root, 50);
        let summary = render_parallel_health_summary(
            &preflight,
            &host_warnings,
            Some("2 completed task(s); see RECEIPTS-DRIFT.md"),
            &["lane-1 TASK-1".to_string(), "lane-3 TASK-3".to_string()],
            &["lane-2 TASK-2".to_string()],
        );
        assert_eq!(
            preflight,
            vec!["agent-browser".to_string(), "docker compose".to_string()]
        );
        assert_eq!(
            host_warnings.len(),
            2,
            "host warnings should be de-duplicated with source freshness"
        );
        assert!(host_warnings[0].contains("live.log"));
        assert!(host_warnings[0].contains("ago"));
        assert!(host_warnings[0].contains("warning: failed syncing host-owned queue state"));
        assert!(host_warnings[1].contains("warning: lane-1 something else"));
        assert!(summary.contains("degraded"));
        assert!(summary.contains("preflight warnings: agent-browser, docker compose"));
        assert!(summary.contains("recent host warnings: live.log"));
        assert!(summary.contains("receipt drift: 2 completed task(s); see RECEIPTS-DRIFT.md"));
        assert!(summary.contains("active recovery lanes: lane-1 TASK-1, lane-3 TASK-3"));
        assert!(summary.contains("stale recovery lanes: lane-2 TASK-2"));

        fs::remove_dir_all(&run_root).expect("failed to remove run root");
    }

    #[test]
    fn lane_repo_process_parser_finds_orphaned_codex_descendants() {
        let lane_repo = PathBuf::from("/tmp/repo/.auto/parallel/lanes/lane-3/repo");
        let ps = r#"
  100 bash /home/r/.local/bin/codex exec --cd /tmp/repo/.auto/parallel/lanes/lane-3/repo -m gpt-5.5
  101 node /home/r/.npm-global/bin/codex exec --cd /tmp/repo/.auto/parallel/lanes/lane-3/repo -m gpt-5.5
  102 rg /tmp/repo/.auto/parallel/lanes/lane-3/repo
  103 bash /home/r/.local/bin/codex exec --cd /tmp/repo/.auto/parallel/lanes/lane-4/repo -m gpt-5.5
"#;

        let pids = parse_lane_repo_process_pids(&lane_repo, ps);

        assert_eq!(pids, vec![100, 101]);
    }

    #[test]
    fn parallel_status_prints_launch_resume_land_safety_verdict() {
        let plan = parse_loop_plan(
            "# IMPLEMENTATION_PLAN\n\n- [ ] `TASK-1` Ready\nDependencies: none\n\n- [ ] `TASK-2` Blocked\nDependencies: `TASK-1`\n",
        );

        let go = parallel_status_safety_verdict(
            &plan,
            &BTreeSet::new(),
            &BTreeMap::new(),
            &BTreeSet::new(),
            true,
            &[],
            &[],
        );
        assert!(go.starts_with("GO:"), "{go}");
        assert!(go.contains("TASK-1"), "{go}");

        let recover = parallel_status_safety_verdict(
            &plan,
            &BTreeSet::new(),
            &BTreeMap::new(),
            &BTreeSet::new(),
            true,
            &[],
            &["lane-2 TASK-2".to_string()],
        );
        assert!(recover.starts_with("RECOVER:"), "{recover}");

        let stop = parallel_status_safety_verdict(
            &plan,
            &BTreeSet::new(),
            &BTreeMap::new(),
            &BTreeSet::new(),
            false,
            &["lane-1 TASK-1".to_string()],
            &[],
        );
        assert!(stop.starts_with("STOP:"), "{stop}");
    }

    #[test]
    fn preflight_classification_does_not_treat_rbtc_mainnet_as_regtest() {
        let repo = unique_temp_dir("parallel-preflight-mainnet");
        fs::create_dir_all(&repo).expect("failed to create temp repo");

        let needs = classify_parallel_preflight_needs(
            "bitino rbtc mainnet settlement proof and wallet signing",
            &repo,
        );
        assert_eq!(
            needs,
            ParallelPreflightNeeds {
                browser: false,
                docker: false,
                regtest: false,
            }
        );

        fs::remove_dir_all(&repo).expect("failed to remove temp repo");
    }

    #[test]
    fn preflight_classification_detects_explicit_regtest_and_browser_infra() {
        let repo = unique_temp_dir("parallel-preflight-regtest");
        fs::create_dir_all(&repo).expect("failed to create temp repo");

        let needs = classify_parallel_preflight_needs(
            "browser smoke against rbtc-regtest with docker compose",
            &repo,
        );
        assert_eq!(
            needs,
            ParallelPreflightNeeds {
                browser: true,
                docker: true,
                regtest: true,
            }
        );

        fs::remove_dir_all(&repo).expect("failed to remove temp repo");
    }

    #[test]
    fn preflight_classification_does_not_treat_compose_prose_as_docker() {
        let repo = unique_temp_dir("parallel-preflight-compose-prose");
        fs::create_dir_all(&repo).expect("failed to create temp repo");

        let needs = classify_parallel_preflight_needs(
            "per-game canvases compose via canvas_prelude rather than re-implementing edge iteration",
            &repo,
        );
        assert_eq!(
            needs,
            ParallelPreflightNeeds {
                browser: false,
                docker: false,
                regtest: false,
            }
        );

        fs::remove_dir_all(&repo).expect("failed to remove temp repo");
    }

    #[test]
    fn linear_usage_limit_error_detection_matches_linear_graphql_payloads() {
        let usage_limit = anyhow!(
            "Linear GraphQL returned errors: [{{\"extensions\":{{\"code\":\"USAGE_LIMIT_EXCEEDED\",\"meta\":{{\"usageMetric\":\"activeIssueCount\"}}}},\"message\":\"usage limit exceeded\"}}]"
        );
        let unrelated = anyhow!("Linear project `demo` not found");

        assert!(is_linear_usage_limit_error(&usage_limit));
        assert!(!is_linear_usage_limit_error(&unrelated));
    }

    #[test]
    fn linear_usage_limit_disables_auto_sync_for_the_rest_of_the_run() {
        let run_root = unique_temp_dir("parallel-linear-usage-limit");
        fs::create_dir_all(&run_root).expect("failed to create run root");
        let logger = ParallelEventLogger::new(&run_root).expect("failed to create logger");
        let err = anyhow!(
            "Linear GraphQL returned errors: [{{\"extensions\":{{\"code\":\"USAGE_LIMIT_EXCEEDED\",\"meta\":{{\"usageMetric\":\"activeIssueCount\"}}}},\"message\":\"You've exceeded the free issue limit for this workspace.\"}}]"
        );
        let mut state = LinearAutoSyncState::default();

        assert!(maybe_disable_linear_auto_sync_for_run(
            &err,
            &mut state,
            &logger,
            "automatic `auto symphony sync --no-ai-planner`",
        ));
        assert!(state.is_disabled());
        assert!(maybe_disable_linear_auto_sync_for_run(
            &err,
            &mut state,
            &logger,
            "automatic `auto symphony sync --no-ai-planner`",
        ));

        let live_log =
            fs::read_to_string(run_root.join("live.log")).expect("failed to read live log");
        assert_eq!(
            live_log
                .matches("disabling further automatic Linear sync for this run")
                .count(),
            1
        );

        fs::remove_dir_all(&run_root).expect("failed to remove run root");
    }

    #[test]
    fn prepare_lane_landing_recovery_rebases_cleanly_when_possible() {
        let (root, remote, upstream, _worker) =
            init_remote_and_clones("parallel-landing-recovery-clean", "main");
        let lane = root.join("lane-clean");
        run_git_in(
            &root,
            [
                "clone",
                "--branch",
                "main",
                remote.to_str().expect("remote path should be utf-8"),
                lane.to_str().expect("lane path should be utf-8"),
            ],
        );
        run_git_in(&lane, ["config", "user.name", "autodev tests"]);
        run_git_in(&lane, ["config", "user.email", "autodev@example.com"]);
        run_git_in(&lane, ["remote", "rename", "origin", "canonical"]);

        let base_commit = git_output(&lane, ["rev-parse", "HEAD"]);
        fs::write(lane.join("lane.txt"), "lane change\n").expect("failed to write lane file");
        run_git_in(&lane, ["add", "lane.txt"]);
        run_git_in(&lane, ["commit", "-m", "lane change"]);

        fs::write(upstream.join("main.txt"), "main change\n").expect("failed to write main file");
        run_git_in(&upstream, ["add", "main.txt"]);
        run_git_in(&upstream, ["commit", "-m", "main change"]);
        run_git_in(&upstream, ["push", "origin", "main"]);
        let remote_head = git_output(&upstream, ["rev-parse", "HEAD"]);

        let mut assignment = ActiveLaneAssignment {
            lane_index: 1,
            attempts: 1,
            task: LoopTask {
                id: "TASK-CLEAN".to_string(),
                title: "clean recovery".to_string(),
                status: LoopTaskStatus::Pending,
                dependencies: Vec::new(),
                estimated_scope: Some("S".to_string()),
                completion_path_target: None,
                lane_kind: LaneKind::Code,
                markdown: "- [ ] `TASK-CLEAN` clean recovery\n".to_string(),
            },
            resumed: false,
            lane_root: root.join("lane-clean-root"),
            lane_repo_root: lane.clone(),
            base_commit: base_commit.clone(),
            stdout_log_path: root.join("lane-clean.stdout.log"),
            stderr_log_path: root.join("lane-clean.stderr.log"),
            worker_pid_path: root.join("lane-clean.worker.pid"),
            clean_commit_since: None,
            terminate_requested_at: None,
            host_recovery_note: None,
        };

        let prep = prepare_lane_landing_recovery(
            &mut assignment,
            "main",
            &base_commit,
            "git cherry-pick failed",
        )
        .expect("landing recovery should prepare");
        assert_eq!(prep, LaneLandingRecoveryPrep::RebasedCleanly);
        assert_eq!(assignment.base_commit, remote_head);
        assert_eq!(run_git_in(&lane, ["status", "--short"]), "");
        assert!(!lane_repo_has_active_cherry_pick(&lane));
        let log = run_git_in(&lane, ["log", "--format=%s", "-2"]);
        assert_eq!(log, "lane change\nmain change\n");

        fs::remove_dir_all(&root).expect("failed to remove temp repo");
    }

    #[test]
    fn cherry_pick_lane_range_treats_empty_tree_diff_as_already_applied() {
        let (root, remote, _upstream, worker) =
            init_remote_and_clones("parallel-empty-lane-commit", "main");
        let lane = root.join("lane-empty");
        run_git_in(
            &root,
            [
                "clone",
                "--branch",
                "main",
                remote.to_str().expect("remote path should be utf-8"),
                lane.to_str().expect("lane path should be utf-8"),
            ],
        );
        run_git_in(&lane, ["config", "user.name", "autodev tests"]);
        run_git_in(&lane, ["config", "user.email", "autodev@example.com"]);

        let base_commit = git_output(&lane, ["rev-parse", "HEAD"]);
        run_git_in(
            &lane,
            ["commit", "--allow-empty", "-m", "verification-only marker"],
        );
        let lane_head = git_output(&lane, ["rev-parse", "HEAD"]);
        run_git_in(
            &worker,
            [
                "fetch",
                lane.to_str().expect("lane path should be utf-8"),
                lane_head.as_str(),
            ],
        );

        cherry_pick_lane_range(
            &worker,
            &base_commit,
            "FETCH_HEAD",
            CherryPickFailurePolicy::Abort,
        )
        .expect("empty tree-diff lane commit should be treated as already applied");

        assert_eq!(git_output(&worker, ["rev-parse", "HEAD"]), base_commit);
        assert_eq!(run_git_in(&worker, ["status", "--short"]), "");
        assert!(!lane_repo_has_active_cherry_pick(&worker));

        fs::remove_dir_all(&root).expect("failed to remove temp repo");
    }

    #[test]
    fn prepare_lane_landing_recovery_squashes_conflict_via_fallback() {
        // Under the cherry-pick fallback (Runner-up #90), recovery prep no
        // longer leaves a conflict mid-state for the worker to resolve.
        // After `threshold` consecutive cherry-pick conflicts the host
        // squashes the lane diff onto the recovery base and reports
        // `RebasedCleanly`. The legacy "leave for worker" path is
        // preserved only when an operator sets the threshold artificially
        // high; the next test in this file covers that escape hatch.
        let (root, remote, upstream, _worker) =
            init_remote_and_clones("parallel-landing-recovery-conflict", "main");
        let lane = root.join("lane-conflict");
        run_git_in(
            &root,
            [
                "clone",
                "--branch",
                "main",
                remote.to_str().expect("remote path should be utf-8"),
                lane.to_str().expect("lane path should be utf-8"),
            ],
        );
        run_git_in(&lane, ["config", "user.name", "autodev tests"]);
        run_git_in(&lane, ["config", "user.email", "autodev@example.com"]);
        run_git_in(&lane, ["remote", "rename", "origin", "canonical"]);

        let base_commit = git_output(&lane, ["rev-parse", "HEAD"]);
        fs::write(lane.join("shared.txt"), "lane version\n").expect("failed to write lane file");
        run_git_in(&lane, ["add", "shared.txt"]);
        run_git_in(&lane, ["commit", "-m", "lane conflict"]);

        fs::write(upstream.join("shared.txt"), "main version\n")
            .expect("failed to write upstream file");
        run_git_in(&upstream, ["add", "shared.txt"]);
        run_git_in(&upstream, ["commit", "-m", "main conflict"]);
        run_git_in(&upstream, ["push", "origin", "main"]);
        let remote_head = git_output(&upstream, ["rev-parse", "HEAD"]);

        let mut assignment = ActiveLaneAssignment {
            lane_index: 2,
            attempts: 1,
            task: LoopTask {
                id: "TASK-CONFLICT".to_string(),
                title: "conflict recovery".to_string(),
                status: LoopTaskStatus::Pending,
                dependencies: Vec::new(),
                estimated_scope: Some("S".to_string()),
                completion_path_target: None,
                lane_kind: LaneKind::Code,
                markdown: "- [ ] `TASK-CONFLICT` conflict recovery\n".to_string(),
            },
            resumed: false,
            lane_root: root.join("lane-conflict-root"),
            lane_repo_root: lane.clone(),
            base_commit: base_commit.clone(),
            stdout_log_path: root.join("lane-conflict.stdout.log"),
            stderr_log_path: root.join("lane-conflict.stderr.log"),
            worker_pid_path: root.join("lane-conflict.worker.pid"),
            clean_commit_since: None,
            terminate_requested_at: None,
            host_recovery_note: None,
        };

        let prep = prepare_lane_landing_recovery(
            &mut assignment,
            "main",
            &base_commit,
            "git cherry-pick failed",
        )
        .expect("landing recovery should prepare");
        assert!(matches!(prep, LaneLandingRecoveryPrep::RebasedCleanly));
        assert_eq!(assignment.base_commit, remote_head);
        // Fallback leaves the worktree clean (squashed commit landed on
        // top of the recovery base) rather than a conflicted cherry-pick.
        assert!(!lane_repo_has_active_cherry_pick(&lane));
        assert_eq!(run_git_in(&lane, ["status", "--short"]), "");
        let head_message = run_git_in(&lane, ["log", "-1", "--format=%s"]);
        assert!(
            head_message.contains("cherry-pick fallback"),
            "squash commit message should mark the fallback, got: {head_message}"
        );

        fs::remove_dir_all(&root).expect("failed to remove temp repo");
    }

    #[test]
    fn superseded_lane_recovery_is_retired_after_newer_task_commit_lands() {
        // Verifies that the superseded-recovery + retire helpers clean up
        // an active cherry-pick when a newer canonical commit for the
        // same task lands upstream. The cherry-pick fallback (Runner-up
        // #90) would normally squash the conflict away from the public
        // entry point (`prepare_lane_landing_recovery`), so we drive the
        // lane into the active-cherry-pick state directly here to keep
        // exercising the retirement codepath those helpers exist for.
        let (root, remote, upstream, _worker) =
            init_remote_and_clones("parallel-superseded-recovery", "main");
        let lane = root.join("lane-superseded");
        run_git_in(
            &root,
            [
                "clone",
                "--branch",
                "main",
                remote.to_str().expect("remote path should be utf-8"),
                lane.to_str().expect("lane path should be utf-8"),
            ],
        );
        run_git_in(&lane, ["config", "user.name", "autodev tests"]);
        run_git_in(&lane, ["config", "user.email", "autodev@example.com"]);
        run_git_in(&lane, ["remote", "rename", "origin", "canonical"]);

        fs::write(lane.join("manifest.json"), "{\"result\":\"old\"}\n")
            .expect("failed to write lane manifest");
        run_git_in(&lane, ["add", "manifest.json"]);
        run_git_in(
            &lane,
            ["commit", "-m", "repo: TASK-001 refresh proof manifest"],
        );
        let lane_head = git_output(&lane, ["rev-parse", "HEAD"]);

        fs::write(upstream.join("manifest.json"), "{\"result\":\"main\"}\n")
            .expect("failed to write upstream manifest");
        run_git_in(&upstream, ["add", "manifest.json"]);
        run_git_in(&upstream, ["commit", "-m", "main conflicting edit"]);
        run_git_in(&upstream, ["push", "origin", "main"]);
        let recovery_base = git_output(&upstream, ["rev-parse", "HEAD"]);

        // Replay the legacy landing-recovery prep manually: fetch the
        // upstream, reset onto it, then start a cherry-pick that will
        // conflict and stay in progress (no `--abort`). The fallback
        // wrapper would squash this away when called via the public
        // recovery prep entry point; bypassing it here lets us assert
        // that the retire helper still cleans up the cherry-pick state.
        run_git_in(&lane, ["fetch", "--quiet", "canonical", "main"]);
        run_git_in(&lane, ["reset", "--hard", &recovery_base]);
        let cherry_pick_status = Command::new("git")
            .arg("-C")
            .arg(&lane)
            .args(["cherry-pick", &lane_head])
            .status()
            .expect("failed to run git cherry-pick");
        assert!(
            !cherry_pick_status.success(),
            "cherry-pick should fail with a conflict so the recovery helpers have state to clean"
        );
        assert!(lane_repo_has_active_cherry_pick(&lane));

        fs::write(upstream.join("manifest.json"), "{\"result\":\"newer\"}\n")
            .expect("failed to write newer upstream manifest");
        run_git_in(&upstream, ["add", "manifest.json"]);
        run_git_in(
            &upstream,
            ["commit", "-m", "repo: TASK-001 publish newer proof"],
        );
        let newer_commit = git_output(&upstream, ["rev-parse", "HEAD"]);

        let superseded = superseded_lane_cherry_pick_recovery(&upstream, &lane, "TASK-001")
            .expect("superseded check should succeed")
            .expect("expected superseded recovery");
        assert_eq!(superseded.superseding_commit, newer_commit);

        let retired = retire_superseded_lane_cherry_pick_recovery(&upstream, &lane, "TASK-001")
            .expect("retirement should succeed")
            .expect("expected retired recovery");
        assert_eq!(retired.superseding_commit, newer_commit);
        assert!(!lane_repo_has_active_cherry_pick(&lane));
        assert_eq!(run_git_in(&lane, ["status", "--short"]), "");
        assert_eq!(git_output(&lane, ["rev-parse", "HEAD"]), recovery_base);

        fs::remove_dir_all(&root).expect("failed to remove temp repo");
    }

    #[test]
    fn loop_worker_env_respects_override_and_inherited_cargo_jobs() {
        let run_root = unique_temp_dir("loop-worker-env");
        fs::create_dir_all(&run_root).expect("failed to create run root");
        let shared_target = run_root
            .join("shared-cargo-target")
            .to_string_lossy()
            .into_owned();

        let inherited = resolve_loop_worker_env(
            None,
            ParallelCargoTarget::Auto,
            Some("8"),
            None,
            22,
            5,
            true,
            &run_root,
        )
        .unwrap();
        assert!(inherited.extra_env.is_empty());
        assert_eq!(inherited.cargo_jobs_summary, "inherited CARGO_BUILD_JOBS=8");
        assert!(inherited.lane_local_cargo_target);
        assert!(inherited
            .cargo_target_summary
            .as_deref()
            .is_some_and(|summary| summary.contains("lane-local")));

        let overridden = resolve_loop_worker_env(
            Some(3),
            ParallelCargoTarget::Auto,
            Some("8"),
            None,
            22,
            5,
            true,
            &run_root,
        )
        .unwrap();
        assert_eq!(
            overridden.extra_env,
            vec![("CARGO_BUILD_JOBS".to_string(), "3".to_string())]
        );
        assert_eq!(overridden.cargo_jobs_summary, "override CARGO_BUILD_JOBS=3");
        assert!(overridden.lane_local_cargo_target);

        let automatic = resolve_loop_worker_env(
            None,
            ParallelCargoTarget::Auto,
            None,
            None,
            22,
            5,
            true,
            &run_root,
        )
        .unwrap();
        assert_eq!(
            automatic.extra_env,
            vec![("CARGO_BUILD_JOBS".to_string(), "3".to_string())]
        );
        assert_eq!(automatic.cargo_jobs_summary, "auto CARGO_BUILD_JOBS=3");
        assert!(automatic.lane_local_cargo_target);

        let shared = resolve_loop_worker_env(
            None,
            ParallelCargoTarget::Shared,
            None,
            None,
            22,
            5,
            true,
            &run_root,
        )
        .unwrap();
        assert_eq!(
            shared.extra_env,
            vec![
                ("CARGO_TARGET_DIR".to_string(), shared_target),
                ("CARGO_BUILD_JOBS".to_string(), "3".to_string())
            ]
        );
        assert!(!shared.lane_local_cargo_target);

        fs::remove_dir_all(&run_root).expect("failed to remove run root");
    }

    #[test]
    fn loop_worker_env_rejects_zero_cargo_jobs_override() {
        let run_root = unique_temp_dir("loop-worker-env-error");
        fs::create_dir_all(&run_root).expect("failed to create run root");
        let err = resolve_loop_worker_env(
            Some(0),
            ParallelCargoTarget::Auto,
            None,
            None,
            22,
            5,
            true,
            &run_root,
        )
        .unwrap_err();
        assert!(err.to_string().contains("--cargo-build-jobs"));
        fs::remove_dir_all(&run_root).expect("failed to remove run root");
    }

    #[test]
    fn lane_assignment_metadata_rejects_changed_task_body() {
        let lane_root = unique_temp_dir("lane-assignment-body");
        fs::create_dir_all(&lane_root).expect("failed to create lane root");
        let task = LoopTask {
            id: "TASK-001".to_string(),
            title: "Initial".to_string(),
            status: LoopTaskStatus::Pending,
            dependencies: vec![],
            estimated_scope: Some("S".to_string()),
            completion_path_target: None,
            lane_kind: LaneKind::Code,
            markdown: "- [ ] `TASK-001` Initial\nVerification: `cargo test task_one`\nRequired tests: `cargo test task_one`\nDependencies: none\n".to_string(),
        };
        write_lane_assignment_metadata(&lane_root, "main", "abc123", &task)
            .expect("metadata should write");

        let mut changed = task.clone();
        changed.markdown.push_str("Extra body\n");
        let err = validate_lane_assignment_metadata(&lane_root, "main", &changed)
            .expect_err("changed body rejected");
        assert!(format!("{err:#}").contains("task body hash changed"));
        fs::remove_dir_all(lane_root).ok();
    }

    #[test]
    fn lane_assignment_metadata_rejects_changed_dependencies() {
        let lane_root = unique_temp_dir("lane-assignment-deps");
        fs::create_dir_all(&lane_root).expect("failed to create lane root");
        let task = LoopTask {
            id: "TASK-001".to_string(),
            title: "Initial".to_string(),
            status: LoopTaskStatus::Pending,
            dependencies: vec!["TASK-000".to_string()],
            estimated_scope: Some("S".to_string()),
            completion_path_target: None,
            lane_kind: LaneKind::Code,
            markdown: "- [ ] `TASK-001` Initial\nVerification: `cargo test task_one`\nRequired tests: `cargo test task_one`\nDependencies: `TASK-000`\n".to_string(),
        };
        write_lane_assignment_metadata(&lane_root, "main", "abc123", &task)
            .expect("metadata should write");

        let mut changed = task.clone();
        changed.dependencies = vec![];
        let err = validate_lane_assignment_metadata(&lane_root, "main", &changed)
            .expect_err("changed dependencies rejected");
        assert!(format!("{err:#}").contains("dependency hash changed"));
        fs::remove_dir_all(lane_root).ok();
    }

    #[test]
    fn lane_assignment_metadata_rejects_changed_verification_text() {
        let lane_root = unique_temp_dir("lane-assignment-verification");
        fs::create_dir_all(&lane_root).expect("failed to create lane root");
        let task = LoopTask {
            id: "TASK-001".to_string(),
            title: "Initial".to_string(),
            status: LoopTaskStatus::Pending,
            dependencies: vec![],
            estimated_scope: Some("S".to_string()),
            completion_path_target: None,
            lane_kind: LaneKind::Code,
            markdown: "- [ ] `TASK-001` Initial\nVerification: `cargo test task_one`\nRequired tests: `cargo test task_one`\nDependencies: none\n".to_string(),
        };
        write_lane_assignment_metadata(&lane_root, "main", "abc123", &task)
            .expect("metadata should write");

        let mut changed = task.clone();
        changed.markdown = changed
            .markdown
            .replace("cargo test task_one", "cargo test task_two");
        let err = validate_lane_assignment_metadata(&lane_root, "main", &changed)
            .expect_err("changed verification rejected");
        assert!(format!("{err:#}").contains("verification text hash changed"));
        fs::remove_dir_all(lane_root).ok();
    }

    #[test]
    fn loop_worker_env_respects_inherited_cargo_target_dir() {
        let run_root = unique_temp_dir("loop-worker-env-inherited-target");
        fs::create_dir_all(&run_root).expect("failed to create run root");

        let env = resolve_loop_worker_env(
            None,
            ParallelCargoTarget::Auto,
            None,
            Some("/tmp/shared-target"),
            22,
            5,
            true,
            &run_root,
        )
        .expect("worker env should resolve");
        assert_eq!(
            env.extra_env,
            vec![
                (
                    "CARGO_TARGET_DIR".to_string(),
                    "/tmp/shared-target".to_string()
                ),
                ("CARGO_BUILD_JOBS".to_string(), "3".to_string())
            ]
        );
        assert_eq!(
            env.cargo_target_summary,
            Some("/tmp/shared-target".to_string())
        );
        assert!(!env.lane_local_cargo_target);

        fs::remove_dir_all(&run_root).expect("failed to remove run root");
    }

    #[test]
    fn parallel_claude_has_no_implicit_turn_budget() {
        let args = ParallelArgs {
            action: None,
            max_iterations: None,
            max_concurrent_workers: 5,
            cargo_build_jobs: None,
            cargo_target: ParallelCargoTarget::Auto,
            prompt_file: None,
            model: "opus".to_string(),
            reasoning_effort: "xhigh".to_string(),
            branch: None,
            reference_repos: Vec::new(),
            include_siblings: false,
            run_root: None,
            codex_bin: PathBuf::from("codex"),
            claude: true,
            max_turns: None,
            max_retries: 2,
        };

        assert_eq!(effective_parallel_claude_max_turns(&args), None);
    }

    #[test]
    fn prompt_filename_task_id_round_trips() {
        assert_eq!(
            task_id_from_prompt_filename("P-029C-attempt-03-prompt.md"),
            Some("P-029C".to_string())
        );
        assert_eq!(
            task_id_from_prompt_filename("WEB-CRAPS-D-attempt-1-prompt.md"),
            Some("WEB-CRAPS-D".to_string())
        );
        assert_eq!(task_id_from_prompt_filename("stderr.log"), None);
    }

    #[test]
    fn lane_task_id_prefers_metadata_and_falls_back_to_latest_prompt() {
        let lane_root = unique_temp_dir("parallel-lane-task-id");
        fs::create_dir_all(&lane_root).expect("failed to create lane root");
        fs::write(lane_root.join("P-018B-attempt-01-prompt.md"), "")
            .expect("failed to write prompt");
        std::thread::sleep(std::time::Duration::from_millis(10));
        fs::write(lane_root.join("P-021-attempt-02-prompt.md"), "")
            .expect("failed to write prompt");

        assert_eq!(
            read_lane_task_id(&lane_root).expect("lane task id should read"),
            Some("P-021".to_string())
        );

        fs::write(lane_root.join(super::LANE_TASK_ID_FILE), "P-029C\n")
            .expect("failed to write metadata");
        assert_eq!(
            read_lane_task_id(&lane_root).expect("lane task id should read"),
            Some("P-029C".to_string())
        );

        fs::remove_dir_all(&lane_root).expect("failed to remove lane root");
    }

    #[test]
    fn lane_repo_progress_reports_commits_and_dirty_state_independently() {
        let repo = unique_temp_dir("parallel-lane-progress");
        init_git_repo(&repo);
        fs::write(repo.join("file.txt"), "base\n").expect("failed to write base file");
        git_ok(&repo, ["add", "file.txt"]);
        git_ok(&repo, ["commit", "-m", "base"]);
        let base = git_output(&repo, ["rev-parse", "HEAD"]);

        fs::write(repo.join("file.txt"), "dirty\n").expect("failed to dirty file");
        assert_eq!(
            inspect_lane_repo_progress(&repo, &base).expect("progress should inspect"),
            LaneRepoProgress::Dirty("M file.txt".to_string())
        );

        git_ok(&repo, ["add", "file.txt"]);
        git_ok(&repo, ["commit", "-m", "task"]);
        assert_eq!(
            inspect_lane_repo_progress(&repo, &base).expect("progress should inspect"),
            LaneRepoProgress::NewCommits
        );

        fs::write(repo.join("file.txt"), "dirty again\n").expect("failed to dirty file again");
        assert_eq!(
            inspect_lane_repo_progress(&repo, &base).expect("progress should inspect"),
            LaneRepoProgress::NewCommitsWithDirty("M file.txt".to_string())
        );

        fs::remove_dir_all(&repo).expect("failed to remove temp repo");
    }

    #[test]
    fn reset_parallel_lane_root_rehomes_existing_contents() {
        let lane_root = unique_temp_dir("parallel-lane-reset");
        fs::create_dir_all(lane_root.join("repo")).expect("failed to create lane repo");
        fs::write(lane_root.join("repo").join("stale.txt"), "stale")
            .expect("failed to write stale file");

        reset_parallel_lane_root(&lane_root).expect("lane root should reset");

        assert!(lane_root.exists(), "lane root should exist after reset");
        assert!(
            fs::read_dir(&lane_root)
                .expect("lane root should be readable")
                .next()
                .is_none(),
            "lane root should be recreated empty"
        );

        let parent = lane_root.parent().expect("lane root should have parent");
        let prefix = format!(
            "{}.stale-",
            lane_root
                .file_name()
                .expect("lane root should have file name")
                .to_string_lossy()
        );
        let stale_dirs = fs::read_dir(parent)
            .expect("parent should be readable")
            .filter_map(|entry| entry.ok())
            .map(|entry| entry.path())
            .filter(|path| {
                path.file_name()
                    .map(|name| name.to_string_lossy().starts_with(&prefix))
                    .unwrap_or(false)
            })
            .collect::<Vec<_>>();
        assert!(
            stale_dirs.is_empty(),
            "stale lane roots should be pruned after reset"
        );

        fs::remove_dir_all(&lane_root).expect("failed to remove lane root");
    }

    #[test]
    fn resume_candidate_matches_requested_task() {
        let ready_tasks = [
            LoopTask {
                id: "P-019D".to_string(),
                title: "first".to_string(),
                status: LoopTaskStatus::Pending,
                dependencies: Vec::new(),
                estimated_scope: Some("S".to_string()),
                completion_path_target: None,
                lane_kind: LaneKind::Code,
                markdown: String::new(),
            },
            LoopTask {
                id: "P-021".to_string(),
                title: "second".to_string(),
                status: LoopTaskStatus::Pending,
                dependencies: Vec::new(),
                estimated_scope: Some("S".to_string()),
                completion_path_target: None,
                lane_kind: LaneKind::Code,
                markdown: String::new(),
            },
        ];
        let mut resumable = BTreeMap::new();
        resumable.insert(
            2,
            LaneResumeCandidate {
                lane_index: 2,
                task: ready_tasks[1].clone(),
                lane_root: PathBuf::from("/tmp/lane-2"),
                lane_repo_root: PathBuf::from("/tmp/lane-2/repo"),
                base_commit: "abc123".to_string(),
                stdout_log_path: PathBuf::from("/tmp/lane-2/stdout.log"),
                stderr_log_path: PathBuf::from("/tmp/lane-2/stderr.log"),
                worker_pid_path: PathBuf::from("/tmp/lane-2/worker.pid"),
                host_recovery_note: None,
            },
        );
        resumable.insert(
            5,
            LaneResumeCandidate {
                lane_index: 5,
                task: ready_tasks[0].clone(),
                lane_root: PathBuf::from("/tmp/lane-5"),
                lane_repo_root: PathBuf::from("/tmp/lane-5/repo"),
                base_commit: "def456".to_string(),
                stdout_log_path: PathBuf::from("/tmp/lane-5/stdout.log"),
                stderr_log_path: PathBuf::from("/tmp/lane-5/stderr.log"),
                worker_pid_path: PathBuf::from("/tmp/lane-5/worker.pid"),
                host_recovery_note: Some("recover this lane".to_string()),
            },
        );

        let matched = take_resume_candidate_for_task(
            &mut resumable,
            &ready_tasks[0].id,
            &BTreeMap::<usize, ActiveLaneAssignment>::new(),
        )
        .expect("expected a matching resumable lane");
        assert_eq!(matched.0, 5);
        assert_eq!(matched.1.task.id, "P-019D");
        assert_eq!(
            matched.1.host_recovery_note.as_deref(),
            Some("recover this lane")
        );
        assert!(resumable.contains_key(&2));
        assert!(!resumable.contains_key(&5));

        let mut rediscovered = BTreeMap::new();
        rediscovered.insert(
            2,
            LaneResumeCandidate {
                lane_index: 2,
                task: ready_tasks[1].clone(),
                lane_root: PathBuf::from("/tmp/lane-2"),
                lane_repo_root: PathBuf::from("/tmp/lane-2/repo"),
                base_commit: "abc123".to_string(),
                stdout_log_path: PathBuf::from("/tmp/lane-2/stdout.log"),
                stderr_log_path: PathBuf::from("/tmp/lane-2/stderr.log"),
                worker_pid_path: PathBuf::from("/tmp/lane-2/worker.pid"),
                host_recovery_note: None,
            },
        );
        resumable
            .get_mut(&2)
            .expect("lane-2 should remain resumable")
            .host_recovery_note = Some("preserve this note".to_string());
        preserve_resume_recovery_notes(&mut rediscovered, &resumable);
        assert_eq!(
            rediscovered
                .get(&2)
                .and_then(|candidate| candidate.host_recovery_note.as_deref()),
            Some("preserve this note")
        );

        let mut active = BTreeMap::new();
        active.insert(
            2,
            ActiveLaneAssignment {
                lane_index: 2,
                attempts: 1,
                task: ready_tasks[1].clone(),
                resumed: true,
                lane_root: PathBuf::from("/tmp/lane-2"),
                lane_repo_root: PathBuf::from("/tmp/lane-2/repo"),
                base_commit: "abc123".to_string(),
                stdout_log_path: PathBuf::from("/tmp/lane-2/stdout.log"),
                stderr_log_path: PathBuf::from("/tmp/lane-2/stderr.log"),
                worker_pid_path: PathBuf::from("/tmp/lane-2/worker.pid"),
                clean_commit_since: None,
                terminate_requested_at: None,
                host_recovery_note: None,
            },
        );
        assert!(
            take_resume_candidate_for_task(&mut resumable, &ready_tasks[1].id, &active).is_none()
        );
    }

    #[test]
    fn lane_scope_budget_tracks_plan_scope() {
        let xs = LoopTask {
            id: "TASK-XS".to_string(),
            title: "tiny".to_string(),
            status: LoopTaskStatus::Pending,
            dependencies: Vec::new(),
            estimated_scope: Some("XS".to_string()),
            completion_path_target: None,
            lane_kind: LaneKind::Code,
            markdown: String::new(),
        };
        let medium = LoopTask {
            id: "TASK-M".to_string(),
            title: "medium".to_string(),
            status: LoopTaskStatus::Pending,
            dependencies: Vec::new(),
            estimated_scope: Some("M".to_string()),
            completion_path_target: None,
            lane_kind: LaneKind::Code,
            markdown: String::new(),
        };

        assert_eq!(lane_scope_budget(&xs).max_changed_files, 8);
        assert_eq!(lane_scope_budget(&xs).max_package_roots, 1);
        assert_eq!(lane_scope_budget(&medium).max_changed_files, 28);
        assert_eq!(lane_scope_budget(&medium).max_package_roots, 3);
    }

    #[test]
    fn verification_only_tasks_are_detected() {
        let verification_only = LoopTask {
            id: "WEB-CRAPS-C".to_string(),
            title: "checkpoint".to_string(),
            status: LoopTaskStatus::Pending,
            dependencies: vec!["WEB-CRAPS-B".to_string()],
            estimated_scope: Some("S".to_string()),
            completion_path_target: None,
            lane_kind: LaneKind::Code,
            markdown: "- [ ] `WEB-CRAPS-C` Checkpoint\n  Scope boundary: verification only.\n  Acceptance criteria:\n    - pass".to_string(),
        };
        let normal = LoopTask {
            id: "WEB-CRAPS-D".to_string(),
            title: "real work".to_string(),
            status: LoopTaskStatus::Pending,
            dependencies: vec!["WEB-CRAPS-C".to_string()],
            estimated_scope: Some("M".to_string()),
            completion_path_target: None,
            lane_kind: LaneKind::Code,
            markdown: "- [ ] `WEB-CRAPS-D` Real work\n  Scope boundary: state source only.\n  Acceptance criteria:\n    - ship".to_string(),
        };

        assert!(is_verification_only_task(&verification_only));
        assert!(!is_verification_only_task(&normal));
    }

    #[test]
    fn lane_kind_routes_operator_and_evidence_tasks() {
        let plan = parse_loop_plan(
            r#"
- [ ] `OPS-001` Loom key ceremony
  Lane kind: operator
  Verification: `ssh root@loom true`
  Dependencies: none

- [ ] `EVID-001` Refresh receipt
  Lane kind: evidence
  Scope boundary: evidence only.
  Verification: `cargo test receipt_refresh`
  Dependencies: none

- [ ] `CODE-001` Normal code
  Verification: `cargo test code`
  Dependencies: none
"#,
        );
        assert_eq!(plan.task("OPS-001").unwrap().lane_kind, LaneKind::Operator);
        assert_eq!(plan.task("EVID-001").unwrap().lane_kind, LaneKind::Evidence);
        assert_eq!(plan.task("CODE-001").unwrap().lane_kind, LaneKind::Code);

        let verdict = parallel_status_safety_verdict(
            &plan,
            &BTreeSet::new(),
            &BTreeMap::new(),
            &BTreeSet::new(),
            true,
            &[],
            &[],
        );
        assert!(verdict.contains("code lanes ready: CODE-001"));
        assert!(verdict.contains("evidence queue: EVID-001"));
        assert!(verdict.contains("operator queue: OPS-001"));
    }

    #[test]
    fn inferred_mainnet_autonomous_gate_remains_dispatchable_code() {
        let plan = parse_loop_plan(
            r#"
- [ ] `LIVE-001` Autonomous loom mainnet canary
  Verification: `LAUNCH_GATE_AUTHORIZE_REAL_RBTC=1 bash scripts/e2e/canary.sh`
  Scope boundary: fail-closed live mainnet proof; emits AUTO_ENV_BLOCKER when credentials or authorization are absent.
  Dependencies: none

- [ ] `OPS-001` Human signoff ceremony
  Verification: `ssh root@loom true`
  Review/closeout: requires operator approval before any live run.
  Dependencies: none
"#,
        );
        assert_eq!(plan.task("LIVE-001").unwrap().lane_kind, LaneKind::Code);
        assert_eq!(plan.task("OPS-001").unwrap().lane_kind, LaneKind::Operator);

        let verdict = parallel_status_safety_verdict(
            &plan,
            &BTreeSet::new(),
            &BTreeMap::new(),
            &BTreeSet::new(),
            true,
            &[],
            &[],
        );
        assert!(verdict.contains("code lanes ready: LIVE-001"));
        assert!(verdict.contains("operator queue: OPS-001"));
    }

    #[test]
    fn operator_actions_file_records_full_task_contract() {
        let run_root = unique_temp_dir("operator-actions");
        fs::create_dir_all(&run_root).expect("failed to create run root");
        let task = LoopTask {
            id: "POOL-300426-07".to_string(),
            title: "Generate live keypairs".to_string(),
            status: LoopTaskStatus::Pending,
            dependencies: Vec::new(),
            estimated_scope: Some("S".to_string()),
            completion_path_target: None,
            lane_kind: LaneKind::Operator,
            markdown: "- [ ] `POOL-300426-07` Generate live keypairs\n  Lane kind: operator\n  Verification: `ssh root@loom make keys`\n  Dependencies: none\n".to_string(),
        };
        let path = write_operator_actions_for_ready_tasks(&run_root, &[task])
            .expect("operator queue should write");
        let text = fs::read_to_string(&path).expect("operator queue should be readable");
        assert!(text.contains("POOL-300426-07"));
        assert!(text.contains("ssh root@loom make keys"));
        fs::remove_dir_all(&run_root).ok();
    }

    #[test]
    fn parse_loop_plan_tracks_ready_and_blocked_dependencies() {
        let plan = r#"
- [ ] `TASK-001` First task
  Dependencies:
  - None
  Estimated scope: small
- [ ] `TASK-002` Second task
  Dependencies:
  - `TASK-001`
  Estimated scope: medium
- [!] `TASK-003` Blocked task
  Dependencies:
  - `TASK-999`
  Estimated scope: large
- [x] `TASK-004` Completed task
"#;

        let snapshot = parse_loop_plan(plan);
        let queue = snapshot.queue_snapshot();
        assert_eq!(queue.pending_ids, vec!["TASK-001", "TASK-002"]);
        assert_eq!(queue.blocked_ids, vec!["TASK-003"]);
        assert_eq!(
            snapshot
                .ready_tasks(&Default::default())
                .into_iter()
                .map(|task| task.id)
                .collect::<Vec<_>>(),
            vec!["TASK-001"]
        );
    }

    #[test]
    fn parse_loop_plan_skips_merged_placeholder_tasks() {
        let plan = r#"
- [ ] `WEB-CODEGEN-A` Real tranche head
  Dependencies:
  - None
- [ ] `WEB-PAYOUT-TRUTH` Merged into WEB-CODEGEN-A
  Status: This standalone item is kept as a checkbox placeholder for traceability but its work is now folded into WEB-CODEGEN-A above.
  Dependencies:
  - `WEB-CODEGEN-A`
"#;

        let snapshot = parse_loop_plan(plan);
        let queue = snapshot.queue_snapshot();
        assert_eq!(queue.pending_ids, vec!["WEB-CODEGEN-A"]);
        assert!(queue.blocked_ids.is_empty());
        assert_eq!(snapshot.tasks.len(), 2);
        assert_eq!(snapshot.tasks[1].status, LoopTaskStatus::Done);
    }

    #[test]
    fn parse_loop_plan_blocks_deferred_not_shipped_rows() {
        let plan = r#"
- [ ] `TASK-A` Implement deferred queue handling
  Dependencies:
  - None
- [ ] `TASK-D` Future feature — **DEFERRED, not shipped**
  Dependencies:
  - None
- [ ] `TASK-E` Depends on deferred feature
  Dependencies:
  - `TASK-D`
"#;

        let snapshot = parse_loop_plan(plan);
        let queue = snapshot.queue_snapshot();
        assert_eq!(queue.pending_ids, vec!["TASK-A", "TASK-E"]);
        assert_eq!(queue.blocked_ids, vec!["TASK-D"]);
        assert_eq!(
            snapshot
                .tasks
                .iter()
                .find(|task| task.id == "TASK-D")
                .map(|task| task.status),
            Some(LoopTaskStatus::Blocked)
        );
        assert_eq!(
            snapshot
                .ready_tasks(&Default::default())
                .into_iter()
                .map(|task| task.id)
                .collect::<Vec<_>>(),
            vec!["TASK-A"]
        );
    }

    #[test]
    fn parse_loop_plan_treats_none_dependencies_as_empty() {
        let plan = r#"
- [ ] `WEB-HOUSE-AUDIT` Audit
  Dependencies: none (parallel with `WEB-CODEGEN-A`)
  Estimated scope: S
- [ ] `WEB-CODEGEN-A` Real tranche head
  Dependencies: `WEB-HOUSE-AUDIT`
  Estimated scope: L
"#;

        let snapshot = parse_loop_plan(plan);
        assert!(snapshot.tasks[0].dependencies.is_empty());
        assert_eq!(snapshot.tasks[1].dependencies, vec!["WEB-HOUSE-AUDIT"]);
        assert_eq!(
            snapshot
                .ready_tasks(&Default::default())
                .into_iter()
                .map(|task| task.id)
                .collect::<Vec<_>>(),
            vec!["WEB-HOUSE-AUDIT"]
        );
    }

    #[test]
    fn parse_loop_plan_ignores_parallelism_notes_in_dependency_lines() {
        let plan = r#"
- [x] `WEB-HOUSE-AUDIT` Audit
  Dependencies: none
  Estimated scope: S
- [x] `WEB-CHANNEL-COVERAGE` Coverage
  Dependencies: none
  Estimated scope: S
- [ ] `WEB-CODEGEN-A` Codegen
  Dependencies: `WEB-HOUSE-AUDIT`, `WEB-CHANNEL-COVERAGE`
  Estimated scope: L
- [ ] `WEB-CLIENT-BUILD` Build
  Dependencies: `WEB-HOUSE-AUDIT`, `WEB-CHANNEL-COVERAGE` (Wave 0 gate — finding #3; parallel with `WEB-CODEGEN-A` + `WEB-DESIGN-SYSTEM`)
  Estimated scope: M
- [ ] `WEB-DESIGN-SYSTEM` Design
  Dependencies: `WEB-CLIENT-BUILD` (need bundle for shell exports), `WEB-HOUSE-AUDIT`, `WEB-CHANNEL-COVERAGE` (Wave 0 gate — finding #3). Parallel with `WEB-CODEGEN-A`.
  Estimated scope: L
"#;

        let snapshot = parse_loop_plan(plan);
        let codegen = snapshot
            .tasks
            .iter()
            .find(|task| task.id == "WEB-CODEGEN-A")
            .expect("WEB-CODEGEN-A present");
        let build = snapshot
            .tasks
            .iter()
            .find(|task| task.id == "WEB-CLIENT-BUILD")
            .expect("WEB-CLIENT-BUILD present");
        let design = snapshot
            .tasks
            .iter()
            .find(|task| task.id == "WEB-DESIGN-SYSTEM")
            .expect("WEB-DESIGN-SYSTEM present");

        assert_eq!(
            codegen.dependencies,
            vec!["WEB-HOUSE-AUDIT", "WEB-CHANNEL-COVERAGE"]
        );
        assert_eq!(
            build.dependencies,
            vec!["WEB-HOUSE-AUDIT", "WEB-CHANNEL-COVERAGE"]
        );
        assert_eq!(
            design.dependencies,
            vec![
                "WEB-CLIENT-BUILD",
                "WEB-HOUSE-AUDIT",
                "WEB-CHANNEL-COVERAGE"
            ]
        );
    }

    #[test]
    fn parse_loop_plan_treats_partial_tasks_as_unfinished_dependencies() {
        let plan = r#"
- [~] `TASK-001` Evidence gap
  Dependencies: none
  Estimated scope: S
- [ ] `TASK-002` Depends on partial
  Dependencies: `TASK-001`
  Estimated scope: S
"#;

        let snapshot = parse_loop_plan(plan);
        let queue = snapshot.queue_snapshot();
        assert_eq!(queue.pending_ids, vec!["TASK-001", "TASK-002"]);
        assert!(
            snapshot
                .ready_tasks(&Default::default())
                .into_iter()
                .map(|task| task.id)
                .collect::<Vec<_>>()
                == vec!["TASK-001"]
        );
    }

    #[test]
    fn parse_loop_plan_skips_partial_completion_path_placeholders() {
        let plan = r#"
- [~] `TASK-001` Historical evidence gap. Completion path: `TASK-010`.
  Dependencies: none
  Estimated scope: S
- [ ] `TASK-010` Real follow-on
  Dependencies: none
  Estimated scope: M
- [ ] `TASK-020` Depends on placeholder alias
  Dependencies: `TASK-001`
  Estimated scope: S
"#;

        let snapshot = parse_loop_plan(plan);
        let queue = snapshot.queue_snapshot();
        assert_eq!(queue.pending_ids, vec!["TASK-010", "TASK-020"]);
        assert_eq!(
            snapshot
                .ready_tasks(&Default::default())
                .into_iter()
                .map(|task| task.id)
                .collect::<Vec<_>>(),
            vec!["TASK-010"]
        );
    }

    #[test]
    fn parse_loop_plan_skips_partial_prose_completion_path_placeholders() {
        let plan = r#"
- [~] `TASK-001` Historical evidence gap. Reconciled via `TASK-099` (see `TASK-010` for the completion path).
  Dependencies: none
  Estimated scope: S
- [ ] `TASK-010` Real follow-on
  Dependencies: none
  Estimated scope: M
- [ ] `TASK-020` Depends on placeholder alias
  Dependencies: `TASK-001`
  Estimated scope: S
"#;

        let snapshot = parse_loop_plan(plan);
        let queue = snapshot.queue_snapshot();
        assert_eq!(queue.pending_ids, vec!["TASK-010", "TASK-020"]);
        assert_eq!(
            snapshot
                .ready_tasks(&Default::default())
                .into_iter()
                .map(|task| task.id)
                .collect::<Vec<_>>(),
            vec!["TASK-010"]
        );
    }

    #[test]
    fn ready_parallel_tasks_skips_partials_deferred_for_this_run() {
        let plan = r#"
- [~] `TASK-001` Evidence gap still needs follow-up
  Dependencies: none
  Estimated scope: S
- [ ] `TASK-002` Independent ready task
  Dependencies: none
  Estimated scope: S
"#;

        let snapshot = parse_loop_plan(plan);
        let ready = ready_parallel_tasks(
            &snapshot,
            &BTreeSet::new(),
            &BTreeMap::new(),
            &BTreeSet::from(["TASK-001".to_string()]),
        );
        assert_eq!(
            ready.into_iter().map(|task| task.id).collect::<Vec<_>>(),
            vec!["TASK-002"]
        );
    }

    #[test]
    fn ready_parallel_tasks_prioritizes_pending_before_partial_followups() {
        let plan = r#"
- [~] `TASK-001` Evidence gap still needs follow-up
  Dependencies: none
  Estimated scope: S
- [ ] `TASK-002` Fresh ready task
  Dependencies: none
  Estimated scope: S
- [~] `TASK-003` Another partial
  Dependencies: none
  Estimated scope: S
"#;

        let snapshot = parse_loop_plan(plan);
        let ready = ready_parallel_tasks(
            &snapshot,
            &BTreeSet::new(),
            &BTreeMap::new(),
            &BTreeSet::new(),
        );
        assert_eq!(
            ready.into_iter().map(|task| task.id).collect::<Vec<_>>(),
            vec!["TASK-002", "TASK-001", "TASK-003"]
        );
    }

    #[test]
    fn prioritize_ready_parallel_tasks_avoids_canonical_dirty_paths() {
        let repo = unique_temp_dir("parallel-ready-priority");
        init_git_repo(&repo);
        fs::write(repo.join("src.txt"), "base\n").expect("failed to write src file");
        run_git_in(&repo, ["add", "src.txt"]);
        run_git_in(&repo, ["commit", "-m", "initial"]);
        fs::write(repo.join("src.txt"), "dirty\n").expect("failed to dirty src file");

        let ready = vec![
            LoopTask {
                id: "TASK-001".to_string(),
                title: "touches dirty file".to_string(),
                status: LoopTaskStatus::Pending,
                dependencies: Vec::new(),
                estimated_scope: Some("S".to_string()),
                completion_path_target: None,
                lane_kind: LaneKind::Code,
                markdown: "- [ ] `TASK-001`\n  Owns: `src.txt`\n".to_string(),
            },
            LoopTask {
                id: "TASK-002".to_string(),
                title: "clean task".to_string(),
                status: LoopTaskStatus::Pending,
                dependencies: Vec::new(),
                estimated_scope: Some("S".to_string()),
                completion_path_target: None,
                lane_kind: LaneKind::Code,
                markdown: "- [ ] `TASK-002`\n  Owns: `docs/proof.md`\n".to_string(),
            },
        ];

        let ordered = prioritize_ready_parallel_tasks(&repo, ready);
        assert_eq!(
            ordered.into_iter().map(|task| task.id).collect::<Vec<_>>(),
            vec!["TASK-002", "TASK-001"]
        );

        fs::remove_dir_all(&repo).expect("failed to remove temp repo");
    }

    #[test]
    fn record_partial_follow_up_gives_one_retry_then_parks() {
        let mut attempted = BTreeSet::new();
        let mut deferred = BTreeSet::new();

        assert_eq!(
            record_partial_follow_up("TASK-001", &mut attempted, &mut deferred),
            PartialFollowUpDisposition::RetryLaterThisRun
        );
        assert!(attempted.contains("TASK-001"));
        assert!(!deferred.contains("TASK-001"));

        assert_eq!(
            record_partial_follow_up("TASK-001", &mut attempted, &mut deferred),
            PartialFollowUpDisposition::ParkForRestOfRun
        );
        assert!(attempted.contains("TASK-001"));
        assert!(deferred.contains("TASK-001"));

        clear_partial_follow_up_tracking("TASK-001", &mut attempted, &mut deferred);
        assert!(!attempted.contains("TASK-001"));
        assert!(!deferred.contains("TASK-001"));
    }

    #[test]
    fn completion_path_alias_resolves_once_follow_on_is_done() {
        let plan = r#"
- [~] `TASK-001` Historical evidence gap. Completion path: `TASK-010`.
  Dependencies: none
  Estimated scope: S
- [x] `TASK-010` Real follow-on
  Dependencies: none
  Estimated scope: M
- [ ] `TASK-020` Depends on placeholder alias
  Dependencies: `TASK-001`
  Estimated scope: S
"#;

        let snapshot = parse_loop_plan(plan);
        let queue = snapshot.queue_snapshot();
        assert_eq!(queue.pending_ids, vec!["TASK-020"]);
        assert_eq!(
            snapshot
                .ready_tasks(&Default::default())
                .into_iter()
                .map(|task| task.id)
                .collect::<Vec<_>>(),
            vec!["TASK-020"]
        );
    }

    #[test]
    fn update_task_completion_in_plan_text_marks_partial_instead_of_dropping_block() {
        let plan = r#"- [ ] `TASK-001` First task
  Dependencies:
  - None
  Estimated scope: small
- [ ] `TASK-002` Second task
  Dependencies:
  - `TASK-001`
  Estimated scope: medium
"#;

        let updated =
            update_task_completion_in_plan_text(plan, "TASK-001", LoopTaskStatus::Partial);

        assert!(updated.contains("- [~] `TASK-001` First task"));
        assert!(updated.contains("TASK-002"));
        assert!(updated.starts_with("- [~] `TASK-001`"));
    }

    #[test]
    fn update_task_completion_in_plan_text_does_not_demote_existing_done_rows() {
        // Two rows share the same task ID (duplicate-ID harvest residue). When
        // a lane lands the still-pending row and reconcile writes Partial, the
        // already-completed sibling must remain `[x]`.
        let plan = r#"- [x] `AUDIT-94` Already completed sibling
  Dependencies: none
  Estimated scope: small
- [ ] `AUDIT-94` Newly assigned duplicate-id row
  Dependencies: none
  Estimated scope: small
"#;

        let updated = update_task_completion_in_plan_text(plan, "AUDIT-94", LoopTaskStatus::Partial);

        assert!(
            updated.contains("- [x] `AUDIT-94` Already completed sibling"),
            "completed sibling must not be demoted: {updated}"
        );
        assert!(
            updated.contains("- [~] `AUDIT-94` Newly assigned duplicate-id row"),
            "still-pending duplicate must be marked partial: {updated}"
        );
    }

    #[test]
    fn audit_parallel_completion_drift_warns_without_demoting_plan() {
        let repo = unique_temp_dir("parallel-drift-audit");
        let run_root = unique_temp_dir("parallel-drift-audit-run");
        fs::create_dir_all(&repo).expect("failed to create repo dir");
        fs::create_dir_all(&run_root).expect("failed to create run dir");
        let plan = "- [x] `TASK-001` First task\n  Dependencies: none\n  Estimated scope: S\n";
        fs::write(repo.join("IMPLEMENTATION_PLAN.md"), plan).expect("failed to write plan");
        let logger = ParallelEventLogger::new(&run_root).expect("logger should initialize");

        let updated = audit_parallel_completion_drift(
            &repo,
            "main",
            &fs::read_to_string(repo.join("IMPLEMENTATION_PLAN.md")).expect("plan should exist"),
            &logger,
        )
        .expect("drift audit should succeed");

        assert_eq!(updated, plan);
        let persisted =
            fs::read_to_string(repo.join("IMPLEMENTATION_PLAN.md")).expect("plan should persist");
        assert_eq!(persisted, plan);
        let triage = fs::read_to_string(repo.join("RECEIPTS-DRIFT.md")).unwrap_or_default();
        assert!(
            triage.contains("TASK-001") && triage.contains("Completed Tasks With Drift"),
            "receipt drift should be report-only, not scheduler work"
        );
        let live_log = fs::read_to_string(run_root.join("live.log"))
            .expect("receipt repair should write host log");
        assert!(live_log.contains("left IMPLEMENTATION_PLAN.md unchanged"));
    }

    #[test]
    fn audit_parallel_completion_drift_logs_only_changed_triage() {
        let repo = unique_temp_dir("parallel-drift-audit-stable-log");
        let run_root = unique_temp_dir("parallel-drift-audit-stable-log-run");
        fs::create_dir_all(&repo).expect("failed to create repo dir");
        fs::create_dir_all(&run_root).expect("failed to create run dir");
        let plan = "- [x] `TASK-001` First task\n  Dependencies: none\n  Estimated scope: S\n";
        fs::write(repo.join("IMPLEMENTATION_PLAN.md"), plan).expect("failed to write plan");
        let logger = ParallelEventLogger::new(&run_root).expect("logger should initialize");

        let updated = audit_parallel_completion_drift(&repo, "main", plan, &logger)
            .expect("first drift audit should succeed");
        assert_eq!(updated, plan);
        let first_log =
            fs::read_to_string(run_root.join("live.log")).expect("first audit should log drift");
        assert!(first_log.contains("left IMPLEMENTATION_PLAN.md unchanged"));

        audit_parallel_completion_drift(&repo, "main", &updated, &logger)
            .expect("second drift audit should succeed");
        let second_log =
            fs::read_to_string(run_root.join("live.log")).expect("second audit should keep log");
        assert_eq!(
            second_log, first_log,
            "unchanged receipt drift should stay visible in RECEIPTS-DRIFT.md without appending another fresh host warning"
        );

        assert!(
            receipt_drift_status_summary(&repo)
                .is_some_and(|summary| summary.contains("1 completed task(s)")),
            "completed receipt drift should remain status noise, not scheduler work"
        );
    }

    #[test]
    fn audit_parallel_completion_drift_backfills_safe_legacy_receipt_footer() {
        let (_root, _remote, repo, _worker) =
            init_remote_and_clones("parallel-drift-backfill", "trunk");
        let run_root = unique_temp_dir("parallel-drift-backfill-run");
        fs::create_dir_all(&run_root).expect("failed to create run dir");
        let plan = "- [x] `TASK-001` First task\n  Verification: `cargo test task_001`\n  Dependencies: none\n  Estimated scope: S\n";
        fs::write(repo.join("IMPLEMENTATION_PLAN.md"), plan).expect("failed to write plan");
        fs::write(repo.join("REVIEW.md"), "## `TASK-001`\n\nComplete.\n")
            .expect("failed to write review");
        let receipt_dir = repo.join(".auto/symphony/verification-receipts");
        fs::create_dir_all(&receipt_dir).expect("failed to create receipt dir");
        fs::write(
            receipt_dir.join("TASK-001.json"),
            r#"{"commands":[{"command":"cargo test task_001","exit_code":0,"status":"passed"}]}"#,
        )
        .expect("failed to write legacy receipt");
        run_git_in(&repo, ["add", "IMPLEMENTATION_PLAN.md", "REVIEW.md"]);
        run_git_in(&repo, ["commit", "-m", "completed task"]);
        run_git_in(&repo, ["push", "origin", "trunk"]);
        let logger = ParallelEventLogger::new(&run_root).expect("logger should initialize");

        let updated = audit_parallel_completion_drift(&repo, "trunk", plan, &logger)
            .expect("drift audit should backfill receipt footer");

        assert_eq!(updated, plan);
        assert!(!repo.join("RECEIPTS-DRIFT.md").exists());
        let log = git_output(&repo, ["log", "-1", "--format=%B"]);
        assert!(log.contains("Auto-Verification-Receipt-Task: TASK-001"));
        let live_log =
            fs::read_to_string(run_root.join("live.log")).expect("backfill should write host log");
        assert!(live_log.contains("receipt-backfill: footerized 1 completed task receipt(s)"));
    }

    #[test]
    fn repair_parallel_canonical_before_dispatch_ignores_receipt_json_staging() {
        let repo = unique_temp_dir("parallel-ignore-receipt-json");
        let run_root = unique_temp_dir("parallel-ignore-receipt-json-run");
        init_git_repo(&repo);
        fs::create_dir_all(&run_root).expect("failed to create run dir");
        run_git_in(&repo, ["branch", "-M", "trunk"]);
        fs::write(repo.join("README.md"), "# repo\n").expect("failed to write README");
        run_git_in(&repo, ["add", "README.md"]);
        run_git_in(&repo, ["commit", "-m", "init"]);
        let receipt_dir = repo.join(".auto/symphony/verification-receipts");
        fs::create_dir_all(&receipt_dir).expect("failed to create receipt dir");
        fs::write(receipt_dir.join("TASK-001.json"), "{}\n").expect("failed to write receipt");
        let before = git_output(&repo, ["rev-parse", "HEAD"]);
        let logger = ParallelEventLogger::new(&run_root).expect("logger should initialize");

        repair_parallel_canonical_before_dispatch(&repo, "trunk", &logger)
            .expect("receipt JSON staging should not force a checkpoint");

        let after = git_output(&repo, ["rev-parse", "HEAD"]);
        assert_eq!(after, before);
        let status = git_output(&repo, ["status", "--short", "--untracked-files=all"]);
        assert!(status.contains(".auto/symphony/verification-receipts/TASK-001.json"));
    }

    #[test]
    fn audit_parallel_completion_drift_reports_closeout_candidates_without_promoting_plan() {
        let repo = unique_temp_dir("parallel-closeout-audit");
        let run_root = unique_temp_dir("parallel-closeout-audit-run");
        fs::create_dir_all(&repo).expect("failed to create repo dir");
        fs::create_dir_all(&run_root).expect("failed to create run dir");
        let plan = "- [~] `TASK-001` First task\n  Dependencies: none\n  Estimated scope: S\n";
        fs::write(repo.join("IMPLEMENTATION_PLAN.md"), plan).expect("failed to write plan");
        fs::write(repo.join("REVIEW.md"), "## `TASK-001`\n\nComplete.\n")
            .expect("failed to write review");
        let logger = ParallelEventLogger::new(&run_root).expect("logger should initialize");

        let updated = audit_parallel_completion_drift(
            &repo,
            "main",
            &fs::read_to_string(repo.join("IMPLEMENTATION_PLAN.md")).expect("plan should exist"),
            &logger,
        )
        .expect("drift audit should succeed");

        assert!(updated.starts_with("- [x] `TASK-001`"));
        let persisted =
            fs::read_to_string(repo.join("IMPLEMENTATION_PLAN.md")).expect("plan should persist");
        assert_eq!(persisted, updated);
        let triage = fs::read_to_string(repo.join("RECEIPTS-DRIFT.md")).unwrap_or_default();
        assert!(
            triage.is_empty() || triage.contains("No repo-local receipt drift detected."),
            "closeout should not leave actionable receipt drift"
        );
        let live_log =
            fs::read_to_string(run_root.join("live.log")).expect("closeout should write host log");
        assert!(live_log.contains("receipt-closeout: closed 1 partial task(s)"));
    }

    #[test]
    fn iteration_prompt_injects_actionable_and_blocked_tasks() {
        let queue = LoopQueueSnapshot {
            pending_ids: vec!["META-001".to_string(), "GATE-P4".to_string()],
            blocked_ids: vec!["DEC-001".to_string()],
        };
        let prompt = build_iteration_prompt("base prompt", &queue);

        assert!(prompt.contains("First actionable unfinished task: `META-001`"));
        assert!(prompt.contains("Unfinished task count: 2"));
        assert!(prompt.contains("Blocked tasks marked `- [!]` to skip this iteration: DEC-001"));
    }

    #[test]
    fn discovers_sibling_git_repos_by_default() {
        let workspace = unique_temp_dir("loop-siblings");
        let repo_root = workspace.join("bitpoker");
        let sibling_repo = workspace.join("robopokermulti");
        let non_repo = workspace.join("notes");

        init_git_repo(&repo_root);
        init_git_repo(&sibling_repo);
        fs::create_dir_all(&non_repo).expect("failed to create non-repo dir");

        let discovered = discover_sibling_git_repos(&repo_root).expect("should discover siblings");

        assert_eq!(
            discovered,
            vec![sibling_repo.canonicalize().expect("canonical sibling")]
        );

        fs::remove_dir_all(&workspace).expect("failed to remove temp workspace");
    }

    #[test]
    fn resolve_reference_repos_merges_siblings_and_explicit_paths() {
        let workspace = unique_temp_dir("loop-reference-merge");
        let repo_root = workspace.join("bitpoker");
        let sibling_repo = workspace.join("robopokermulti");
        let explicit_repo = workspace.join("sharedlib");

        init_git_repo(&repo_root);
        init_git_repo(&sibling_repo);
        init_git_repo(&explicit_repo);

        let resolved = resolve_reference_repos(
            &repo_root,
            &[PathBuf::from("../sharedlib"), sibling_repo.clone()],
            true,
        )
        .expect("should resolve sibling and explicit repos");

        assert_eq!(
            resolved,
            vec![
                sibling_repo.canonicalize().expect("canonical sibling"),
                explicit_repo.canonicalize().expect("canonical explicit"),
            ]
        );

        fs::remove_dir_all(&workspace).expect("failed to remove temp workspace");
    }

    fn init_git_repo(path: &PathBuf) {
        fs::create_dir_all(path).expect("failed to create repo dir");
        let status = Command::new("git")
            .args(["init", "-q"])
            .arg(path)
            .status()
            .expect("failed to run git init");
        assert!(status.success(), "git init should succeed");
        git_ok(path, ["config", "user.email", "test@example.com"]);
        git_ok(path, ["config", "user.name", "Autodev Test"]);
    }

    fn run_git_in<'a>(repo: &std::path::Path, args: impl IntoIterator<Item = &'a str>) -> String {
        let output = Command::new("git")
            .arg("-C")
            .arg(repo)
            .args(args)
            .output()
            .expect("failed to launch git");
        assert!(
            output.status.success(),
            "git failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout).expect("git stdout should be utf-8")
    }

    fn init_remote_and_clones(name: &str, branch: &str) -> (PathBuf, PathBuf, PathBuf, PathBuf) {
        let root = unique_temp_dir(name);
        let remote = root.join("remote.git");
        let upstream = root.join("upstream");
        let worker = root.join("worker");

        fs::create_dir_all(&root).expect("failed to create temp root");
        run_git_in(
            &root,
            [
                "init",
                "--bare",
                remote.to_str().expect("remote path should be utf-8"),
            ],
        );
        run_git_in(
            &root,
            [
                "clone",
                remote.to_str().expect("remote path should be utf-8"),
                upstream.to_str().expect("upstream path should be utf-8"),
            ],
        );
        run_git_in(&upstream, ["config", "user.name", "autodev tests"]);
        run_git_in(&upstream, ["config", "user.email", "autodev@example.com"]);
        fs::write(upstream.join("README.md"), "# init\n").expect("failed to write README");
        run_git_in(&upstream, ["add", "README.md"]);
        run_git_in(&upstream, ["commit", "-m", "init"]);
        run_git_in(&upstream, ["branch", "-M", branch]);
        run_git_in(&upstream, ["push", "-u", "origin", branch]);

        run_git_in(
            &root,
            [
                "clone",
                "--branch",
                branch,
                remote.to_str().expect("remote path should be utf-8"),
                worker.to_str().expect("worker path should be utf-8"),
            ],
        );
        run_git_in(&worker, ["config", "user.name", "autodev tests"]);
        run_git_in(&worker, ["config", "user.email", "autodev@example.com"]);

        (root, remote, upstream, worker)
    }

    fn git_ok<const N: usize>(repo: &PathBuf, args: [&str; N]) {
        let output = Command::new("git")
            .arg("-C")
            .arg(repo)
            .args(args)
            .output()
            .expect("failed to run git");
        assert!(
            output.status.success(),
            "git command failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    fn git_output<const N: usize>(repo: &PathBuf, args: [&str; N]) -> String {
        let output = Command::new("git")
            .arg("-C")
            .arg(repo)
            .args(args)
            .output()
            .expect("failed to run git");
        assert!(
            output.status.success(),
            "git command failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8_lossy(&output.stdout).trim().to_string()
    }

    fn unique_temp_dir(label: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("time went backwards")
            .as_nanos();
        std::env::temp_dir().join(format!("autodev-{label}-{nanos}"))
    }


    fn set_file_mtime_epoch(path: &std::path::Path) {
        let status = Command::new("touch")
            .args(["-d", "@1"])
            .arg(path)
            .status()
            .expect("failed to run touch");
        assert!(status.success(), "touch should update test file mtime");
    }

    // ---- Change #9: receipts + plan-integrity ------------------------

    #[test]
    fn detect_plan_demotions_flags_done_to_pending() {
        let before = "- [x] `TASK-1` first\n- [x] `TASK-2` second\n";
        let after = "- [ ] `TASK-1` first\n- [x] `TASK-2` second\n";
        let report = super::detect_plan_demotions(before, after);
        assert_eq!(report.demoted_task_ids, vec!["TASK-1".to_string()]);
    }

    #[test]
    fn detect_plan_demotions_flags_done_to_partial() {
        let before = "- [x] `AUDIT-7` resolved\n";
        let after = "- [~] `AUDIT-7` resolved\n";
        let report = super::detect_plan_demotions(before, after);
        assert_eq!(report.demoted_task_ids, vec!["AUDIT-7".to_string()]);
    }

    #[test]
    fn detect_plan_demotions_ignores_promotions_and_new_rows() {
        let before = "- [ ] `TASK-1`\n";
        let after = "- [x] `TASK-1`\n- [ ] `TASK-2`\n";
        let report = super::detect_plan_demotions(before, after);
        assert!(report.is_empty());
    }

    #[test]
    fn assert_no_plan_demotion_rejects_demotion_against_head() {
        let repo = unique_temp_dir("plan-demotion-guard");
        fs::create_dir_all(&repo).expect("mkdir");
        run_git_in(&repo, ["init", "--quiet", "-b", "main"]);
        run_git_in(&repo, ["config", "user.email", "t@example.com"]);
        run_git_in(&repo, ["config", "user.name", "Autodev Test"]);
        fs::write(
            repo.join("IMPLEMENTATION_PLAN.md"),
            "- [x] `TASK-1` done\n",
        )
        .expect("write plan");
        run_git_in(&repo, ["add", "IMPLEMENTATION_PLAN.md"]);
        run_git_in(&repo, ["commit", "-m", "init"]);
        fs::write(
            repo.join("IMPLEMENTATION_PLAN.md"),
            "- [ ] `TASK-1` done\n",
        )
        .expect("rewrite plan");
        let err = super::assert_no_plan_demotion(&repo, "HEAD")
            .expect_err("demotion should be refused");
        let msg = format!("{err:#}");
        assert!(msg.contains("TASK-1"), "error should name TASK-1: {msg}");
        fs::remove_dir_all(&repo).expect("cleanup");
    }

    #[test]
    fn assert_no_plan_demotion_allows_clean_changes() {
        let repo = unique_temp_dir("plan-demotion-clean");
        fs::create_dir_all(&repo).expect("mkdir");
        run_git_in(&repo, ["init", "--quiet", "-b", "main"]);
        run_git_in(&repo, ["config", "user.email", "t@example.com"]);
        run_git_in(&repo, ["config", "user.name", "Autodev Test"]);
        fs::write(
            repo.join("IMPLEMENTATION_PLAN.md"),
            "- [ ] `TASK-1` open\n",
        )
        .expect("write plan");
        run_git_in(&repo, ["add", "IMPLEMENTATION_PLAN.md"]);
        run_git_in(&repo, ["commit", "-m", "init"]);
        fs::write(
            repo.join("IMPLEMENTATION_PLAN.md"),
            "- [x] `TASK-1` open\n",
        )
        .expect("rewrite plan");
        super::assert_no_plan_demotion(&repo, "HEAD")
            .expect("promotion should be allowed");
        fs::remove_dir_all(&repo).expect("cleanup");
    }

    #[test]
    fn receipts_rehash_amend_embeds_anchor_footer() {
        let repo = unique_temp_dir("rehash-amend");
        fs::create_dir_all(&repo).expect("mkdir");
        run_git_in(&repo, ["init", "--quiet", "-b", "main"]);
        run_git_in(&repo, ["config", "user.email", "t@example.com"]);
        run_git_in(&repo, ["config", "user.name", "Autodev Test"]);
        fs::write(repo.join("plan.md"), "body\n").expect("write");
        run_git_in(&repo, ["add", "plan.md"]);
        run_git_in(&repo, ["commit", "-m", "lane: land plan"]);

        super::receipts_rehash_amend(&repo, &[PathBuf::from("plan.md")])
            .expect("rehash amend");

        let body = run_git_in(&repo, ["log", "-1", "--format=%B"]);
        assert!(
            body.contains(super::receipts::RECEIPT_ANCHOR_COMMIT_KEY),
            "footer missing commit key: {body}"
        );
        assert!(
            body.contains(super::receipts::RECEIPT_ANCHOR_CONTENT_KEY),
            "footer missing content key: {body}"
        );
        fs::remove_dir_all(&repo).expect("cleanup");
    }

    // ---- Runner-up #90: cherry-pick + apply-patch fallback ------------

    #[test]
    fn cherry_pick_fallback_squashes_after_threshold() {
        let root = unique_temp_dir("parallel-fallback-squash");
        let lane_dir = root.join("lane-fallback");
        fs::create_dir_all(&lane_dir).expect("mkdir");
        run_git_in(&lane_dir, ["init", "--quiet", "-b", "main"]);
        run_git_in(&lane_dir, ["config", "user.email", "t@example.com"]);
        run_git_in(&lane_dir, ["config", "user.name", "Autodev"]);
        fs::write(lane_dir.join("shared.txt"), "base\n").expect("write");
        run_git_in(&lane_dir, ["add", "shared.txt"]);
        run_git_in(&lane_dir, ["commit", "-m", "base"]);
        let base = run_git_in(&lane_dir, ["rev-parse", "HEAD"]).trim().to_string();

        run_git_in(&lane_dir, ["checkout", "-b", "lane"]);
        fs::write(lane_dir.join("shared.txt"), "lane edit\n").expect("write");
        run_git_in(&lane_dir, ["commit", "-am", "lane change"]);
        let _lane_head = run_git_in(&lane_dir, ["rev-parse", "HEAD"]).trim().to_string();

        run_git_in(&lane_dir, ["checkout", "main"]);
        fs::write(lane_dir.join("shared.txt"), "main edit\n").expect("write");
        run_git_in(&lane_dir, ["commit", "-am", "main change"]);
        let parent = run_git_in(&lane_dir, ["rev-parse", "HEAD"]).trim().to_string();

        let outcome = super::cherry_pick_lane_range_with_fallback(
            &lane_dir, &base, &parent, "lane", 1,
        )
        .expect("fallback should land");
        match outcome {
            super::CherryPickFallbackOutcome::Squashed { conflicts_seen } => {
                assert!(conflicts_seen >= 1);
            }
            super::CherryPickFallbackOutcome::CherryPicked => {
                panic!("expected fallback to engage");
            }
        }
        let body = run_git_in(&lane_dir, ["log", "-1", "--format=%s"]);
        assert!(
            body.contains("cherry-pick fallback"),
            "squash commit message should mark the fallback: {body}"
        );

        fs::remove_dir_all(&root).expect("cleanup");
    }

    #[test]
    fn apply_patch_fallback_exact_match() {
        let source = "alpha\nbeta\ngamma\n";
        let (out, outcome) = super::apply_patch_with_structural_fallback(
            source,
            &["beta"],
            &["delta"],
            &["alpha"],
            &["gamma"],
        );
        assert_eq!(outcome, super::StructuralPatchOutcome::AppliedExact);
        assert!(out.contains("alpha"));
        assert!(out.contains("delta"));
        assert!(!out.contains("beta"));
    }

    #[test]
    fn apply_patch_fallback_structural_match_when_expected_drifts() {
        // expected lines carry line-number prefixes so the exact match
        // fails; the surrounding context still anchors a structural splice.
        let source = "alpha\nbeta\nbeta2\ngamma\n";
        let (out, outcome) = super::apply_patch_with_structural_fallback(
            source,
            &["12: beta", "13: beta2"],
            &["delta"],
            &["alpha"],
            &["gamma"],
        );
        assert!(
            matches!(
                outcome,
                super::StructuralPatchOutcome::AppliedStructural { .. }
            ),
            "expected structural fallback, got {outcome:?}"
        );
        assert!(out.contains("alpha"));
        assert!(out.contains("delta"));
        assert!(out.contains("gamma"));
        assert!(!out.contains("beta\n"));
        assert!(!out.contains("beta2"));
    }

    #[test]
    fn apply_patch_fallback_no_match_returns_source_unchanged() {
        let source = "alpha\nbeta\ngamma\n";
        let (out, outcome) = super::apply_patch_with_structural_fallback(
            source,
            &["nope"],
            &["whatever"],
            &["does-not-exist"],
            &["also-missing"],
        );
        assert_eq!(outcome, super::StructuralPatchOutcome::NoMatch);
        assert_eq!(out, source);
    }

    // ---- Change #8: lane checkpoint round-trip --------------------------

    #[test]
    fn lane_checkpoint_round_trips_at_phase_boundary() {
        let root = unique_temp_dir("lane-checkpoint-round-trip");
        let lane_root = root.join("lane-0");
        fs::create_dir_all(&lane_root).expect("mkdir lane root");

        // First boundary: simulate the spawn-side "analyze" checkpoint.
        super::record_lane_checkpoint(
            &lane_root,
            "analyze",
            serde_json::json!({"task_id": "TASK-1", "attempt": 1}),
        );
        let analyze = super::load_lane_checkpoint(&lane_root)
            .expect("analyze checkpoint must round-trip");
        assert_eq!(analyze.phase, "analyze");
        assert_eq!(
            analyze.blob.get("task_id").and_then(|v| v.as_str()),
            Some("TASK-1")
        );

        // Second boundary: simulate the post-landing "commit" checkpoint
        // overwriting the prior one at the same path.
        super::record_lane_checkpoint(
            &lane_root,
            "commit",
            serde_json::json!({
                "task_id": "TASK-1",
                "landed_head": "deadbeef",
                "completion_status": "Done",
            }),
        );
        let commit = super::load_lane_checkpoint(&lane_root)
            .expect("commit checkpoint must round-trip");
        assert_eq!(commit.phase, "commit");
        assert_eq!(
            commit.blob.get("landed_head").and_then(|v| v.as_str()),
            Some("deadbeef")
        );

        // The checkpoint must land at the documented path so a survived
        // process can read it back without help.
        let path = super::lane_checkpoint_path(&lane_root);
        assert!(path.exists(), "checkpoint file should exist at {}", path.display());
        assert_eq!(path.file_name().and_then(|s| s.to_str()), Some("lane-state.json"));

        fs::remove_dir_all(&root).expect("cleanup");
    }
