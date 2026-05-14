#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn super_findings_round_trips_through_serde() {
        let original = SuperFindings {
            run_id: "20260513-193400".to_string(),
            generated_at: "2026-05-13T19:34:00Z".to_string(),
            readiness: "conditional_go".to_string(),
            blockers: vec![SuperBlocker {
                id: "BLK-001".to_string(),
                title: "audit findings concatenation".to_string(),
                owner_surface: "src/audit_everything.rs".to_string(),
                severity: "high".to_string(),
                evidence: ".auto/audit-everything/20260513/FINAL-REVIEW.md".to_string(),
                remediation_hint: "emit AUDIT-FINDINGS-SUMMARY.json instead".to_string(),
            }],
            risks: vec![SuperRisk {
                id: "RSK-001".to_string(),
                title: "supervisor self-respawn".to_string(),
                likelihood: "med".to_string(),
                impact: "high".to_string(),
                mitigation: "checkpoint launcher state in-tree".to_string(),
            }],
            gates: vec![SuperGate {
                id: "GATE-001".to_string(),
                name: "pre-parallel".to_string(),
                status: "pass".to_string(),
                evidence_paths: vec![".auto/super/run-1/DETERMINISTIC-GATE.json".to_string()],
            }],
            campaign_plan: SuperCampaignPlan {
                horizon_days: 14,
                milestones: vec![
                    SuperMilestone {
                        id: "M-1".to_string(),
                        title: "land synthesis JSON".to_string(),
                        day: 2,
                        depends_on: vec![],
                    },
                    SuperMilestone {
                        id: "M-2".to_string(),
                        title: "wire JSON into execution gate".to_string(),
                        day: 4,
                        depends_on: vec!["M-1".to_string()],
                    },
                ],
            },
            operator_queue: vec![],
            auto_resolved: vec![],
        };

        let serialized =
            serde_json::to_string_pretty(&original).expect("SuperFindings must serialize");
        let deserialized: SuperFindings =
            serde_json::from_str(&serialized).expect("SuperFindings must deserialize");
        assert_eq!(deserialized, original);
    }

    #[test]
    fn super_findings_accepts_missing_optional_collections() {
        let minimal = r#"{
            "run_id": "run-1",
            "generated_at": "2026-05-13T19:34:00Z",
            "readiness": "go",
            "campaign_plan": {"horizon_days": 14}
        }"#;
        let findings: SuperFindings =
            serde_json::from_str(minimal).expect("minimal SuperFindings must deserialize");
        assert!(findings.blockers.is_empty());
        assert!(findings.risks.is_empty());
        assert!(findings.gates.is_empty());
        assert!(findings.campaign_plan.milestones.is_empty());
        assert_eq!(findings.campaign_plan.horizon_days, 14);
    }

    #[test]
    fn build_super_execution_gate_prompt_references_findings_json_not_legacy_bundle() {
        let repo = PathBuf::from("/tmp/super-exec-gate-test/repo");
        let planning = PathBuf::from("/tmp/super-exec-gate-test/repo/genesis");
        let super_root = PathBuf::from("/tmp/super-exec-gate-test/repo/.auto/super/run-1");
        let prompt = build_super_execution_gate_prompt(&repo, &planning, None, &super_root);

        assert!(
            prompt.contains("super-findings.json"),
            "execution-gate prompt must reference canonical super-findings.json: {prompt}"
        );
        assert!(
            prompt.contains(
                "/tmp/super-exec-gate-test/repo/.auto/super/run-1/super-findings.json"
            ),
            "execution-gate prompt must include the absolute findings path: {prompt}"
        );
        for legacy in [
            "CEO-14-DAY-PLAN.md",
            "FUNCTIONAL-REVIEWS.md",
            "PRODUCTION-READINESS.md",
            "RISK-REGISTER.md",
            "QUALITY-GATES.md",
            "SYSTEM-MAP.md",
        ] {
            assert!(
                !prompt.contains(&format!("/{legacy}")),
                "execution-gate prompt must not request retired artifact `{legacy}`: {prompt}"
            );
        }
        assert!(prompt.contains("Verdict: GO"));
        assert!(prompt.contains("Verdict: NO-GO"));
    }

    #[test]
    fn build_super_corpus_review_prompt_emits_findings_and_report_only() {
        let repo = PathBuf::from("/tmp/super-corpus-review-test/repo");
        let planning = PathBuf::from("/tmp/super-corpus-review-test/repo/genesis");
        let super_root = PathBuf::from("/tmp/super-corpus-review-test/repo/.auto/super/run-1");
        let prompt = build_super_corpus_review_prompt(&repo, &planning, &super_root);

        assert!(prompt.contains("super-findings.json"));
        assert!(prompt.contains("SUPER-REPORT.md"));
        // Retired bundle must NOT appear as required output (it should only
        // appear inside the negation clause that tells the model to skip it).
        for legacy in [
            "CEO-14-DAY-PLAN.md",
            "FUNCTIONAL-REVIEWS.md",
            "PRODUCTION-READINESS.md",
            "RISK-REGISTER.md",
            "QUALITY-GATES.md",
            "SYSTEM-MAP.md",
        ] {
            // Only allowed mention: the explicit "do not produce" negation list.
            let count = prompt.matches(legacy).count();
            assert!(
                count <= 1,
                "legacy artifact `{legacy}` referenced {count} times in corpus-review prompt; expected at most one (the negation clause)"
            );
        }
    }

    #[test]
    fn build_super_focus_combines_production_directive_and_prompt() {
        let focus = build_super_focus(Some("ship the CLI"), Some("security first"));
        assert!(focus.contains("new CEO"));
        assert!(focus.contains("14 days"));
        assert!(focus.contains("Perfect design/runtime integrity first"));
        assert!(focus.contains("ship the CLI"));
        assert!(focus.contains("security first"));
    }

    #[test]
    fn deterministic_gate_accepts_scoped_unfinished_task() {
        let root = temp_dir("super-gate-ok");
        let plan = root.join(IMPLEMENTATION_PLAN);
        fs::write(&plan, valid_plan("cargo test super_command::tests::deterministic_gate_accepts_scoped_unfinished_task")).unwrap();
        let summary = verify_parallel_ready_plan(&plan).unwrap();
        assert_eq!(summary.unchecked_tasks, 1);
        assert_eq!(summary.priority_tasks, 1);
    }

    #[test]
    fn deterministic_gate_rejects_package_wide_cargo_test() {
        let root = temp_dir("super-gate-broad");
        let plan = root.join(IMPLEMENTATION_PLAN);
        fs::write(&plan, valid_plan("cargo test")).unwrap();
        let error = verify_parallel_ready_plan(&plan).expect_err("expected broad test rejection");
        assert!(error.to_string().contains("package-wide cargo test"));
    }

    #[test]
    fn super_rejects_task_missing_runtime_ui_fields() {
        let root = temp_dir("super-gate-missing-runtime-ui");
        let plan = root.join(IMPLEMENTATION_PLAN);
        let malformed = valid_plan(
            "cargo test super_command::tests::super_rejects_task_missing_runtime_ui_fields",
        )
        .replace("    Runtime owner: `src/super_command.rs`\n", "");
        fs::write(&plan, malformed).unwrap();

        let error = verify_parallel_ready_plan(&plan)
            .expect_err("expected rich runtime/UI task contract rejection");

        assert!(format!("{error:#}").contains("task `TASK-001` missing `Runtime owner:`"));
    }

    #[test]
    fn super_accepts_generated_rich_task_contract() {
        let root = temp_dir("super-gate-rich-contract");
        let plan = root.join(IMPLEMENTATION_PLAN);
        fs::write(
            &plan,
            valid_plan(
                "cargo test super_command::tests::super_accepts_generated_rich_task_contract",
            ),
        )
        .unwrap();

        let summary = verify_parallel_ready_plan(&plan).unwrap();

        assert_eq!(summary.unchecked_tasks, 1);
        assert_eq!(summary.priority_tasks, 1);
        assert_eq!(summary.follow_on_tasks, 0);
    }

    #[test]
    fn resume_helpers_skip_terminal_stages_and_restore_gate_artifact() {
        let root = temp_dir("super-resume-manifest");
        let artifact = root.join("gen-output");
        let gate = DeterministicGateSummary {
            unchecked_tasks: 3,
            priority_tasks: 2,
            follow_on_tasks: 1,
        };
        fs::write(
            root.join("DETERMINISTIC-GATE.json"),
            serde_json::to_vec_pretty(&gate).unwrap(),
        )
        .unwrap();

        let manifest = SuperManifest {
            run_id: "run-1".to_string(),
            repo_root: "/repo".to_string(),
            planning_root: "/repo/genesis".to_string(),
            output_dir: Some("/repo/gen-out".to_string()),
            super_root: root.display().to_string(),
            prompt: Some("ship it".to_string()),
            focus: Some("market drama".to_string()),
            model: "gpt-5.5".to_string(),
            reasoning_effort: "xhigh".to_string(),
            worker_model: "gpt-5.5".to_string(),
            worker_reasoning_effort: "high".to_string(),
            max_concurrent_workers: 5,
            max_iterations: None,
            execute: true,
            design_enabled: true,
            super_review_skipped: false,
            design_resolve_passes: 3,
            with_audit: false,
            audit_threads: 0,
            audit_first_pass_retries: 0,
            audit_run_id: None,
            branch: Some("main".to_string()),
            reference_repos: vec!["/ref".to_string()],
            binary: "auto test".to_string(),
            stages: vec![
                SuperStage {
                    name: "gen".to_string(),
                    status: "complete".to_string(),
                    artifact: Some(artifact.display().to_string()),
                },
                SuperStage {
                    name: "parallel".to_string(),
                    status: "launched".to_string(),
                    artifact: Some(root.join("parallel").display().to_string()),
                },
            ],
        };
        fs::write(
            root.join("manifest.json"),
            serde_json::to_vec_pretty(&manifest).unwrap(),
        )
        .unwrap();

        let loaded = load_super_manifest(&root).unwrap();

        assert!(super_stage_terminal(&loaded, "gen"));
        assert!(super_stage_terminal(&loaded, "parallel"));
        assert_eq!(super_stage_artifact(&loaded, "gen"), Some(artifact));
        assert_eq!(read_deterministic_gate(&root).unwrap(), gate);
    }

    fn valid_plan(verification: &str) -> String {
        format!(
            r#"# IMPLEMENTATION_PLAN

## Priority Work

- [ ] `TASK-001` Harden super gate

    Spec: `specs/220426-super.md`
    Why now: proves the gate works.
    Codebase evidence: `src/super_command.rs`
    Source of truth: `src/super_command.rs`
    Runtime owner: `src/super_command.rs`
    UI consumers: terminal output
    Generated artifacts: `.auto/super/*/DETERMINISTIC-GATE.json`
    Fixture boundary: production code parses the live root plan, not fixture rows.
    Retired surfaces: legacy active task rows without runtime/UI contract fields.
    Owns: `src/super_command.rs`
    Integration touchpoints: `src/main.rs`
    Scope boundary: do not launch workers.
    Acceptance criteria: scoped plan passes.
    Verification: {verification}
    Required tests: `cargo test super_command::tests::super_accepts_generated_rich_task_contract`
    Contract generation: `cargo test super_command::tests::super_accepts_generated_rich_task_contract`
    Cross-surface tests: `cargo test super_command::tests::super_accepts_generated_rich_task_contract`
    Review/closeout: reviewer checks super and generation task contracts stay aligned.
    Completion artifacts: `src/super_command.rs`
    Lane kind: code
    Dependencies: none
    Estimated scope: S
    Completion signal: tests pass.

## Follow-On Work

## Completed / Already Satisfied
"#
        )
    }

    fn temp_dir(label: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "autodev-{label}-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&path).unwrap();
        path
    }

    fn findings_with_entries(entries: Vec<OperatorQueueEntry>) -> SuperFindings {
        SuperFindings {
            run_id: "run-1".to_string(),
            generated_at: "2026-05-13T19:34:00Z".to_string(),
            readiness: "conditional_go".to_string(),
            blockers: vec![],
            risks: vec![],
            gates: vec![],
            campaign_plan: SuperCampaignPlan {
                horizon_days: 14,
                milestones: vec![],
            },
            operator_queue: entries,
            auto_resolved: vec![],
        }
    }

    #[test]
    fn auto_resolve_baseline_walk_steps_current_toward_floor() {
        let mut findings = findings_with_entries(vec![OperatorQueueEntry {
            id: "OP-1".to_string(),
            title: "walk active_channels baseline".to_string(),
            policy: OperatorPolicy::Deterministic,
            resolver_kind: Some(ResolverKind::BaselineWalk {
                current: 469,
                floor: 256,
            }),
            payload: "active_channels".to_string(),
            evidence: "ops/baselines.json".to_string(),
        }]);
        let resolved = auto_resolve_deterministic_entries(&mut findings, Path::new("/tmp"))
            .expect("resolver must succeed");
        assert_eq!(resolved.len(), 1);
        assert_eq!(resolved[0].new_value, "468");
        assert_eq!(resolved[0].resolver, "baseline_walk");
        assert!(findings.operator_queue.is_empty());
        assert_eq!(findings.auto_resolved.len(), 1);
    }

    #[test]
    fn auto_resolve_baseline_walk_stops_at_floor() {
        let mut findings = findings_with_entries(vec![OperatorQueueEntry {
            id: "OP-1".to_string(),
            title: "walk at floor".to_string(),
            policy: OperatorPolicy::Deterministic,
            resolver_kind: Some(ResolverKind::BaselineWalk {
                current: 100,
                floor: 100,
            }),
            payload: String::new(),
            evidence: String::new(),
        }]);
        let resolved = auto_resolve_deterministic_entries(&mut findings, Path::new("/tmp"))
            .expect("floor walk must succeed");
        assert_eq!(resolved.len(), 1);
        assert_eq!(resolved[0].new_value, "100");
    }

    #[test]
    fn auto_resolve_env_default_reads_spec_file() {
        let root = temp_dir("super-resolver-env");
        let spec = root.join("spec.md");
        fs::write(
            &spec,
            "# defaults\n- RAT_WIRE_ASCII=1\n- RAT_WIRE_MOTION=on\n",
        )
        .unwrap();
        let mut findings = findings_with_entries(vec![OperatorQueueEntry {
            id: "OP-1".to_string(),
            title: "env default".to_string(),
            policy: OperatorPolicy::Deterministic,
            resolver_kind: Some(ResolverKind::EnvDefault {
                var: "RAT_WIRE_ASCII".to_string(),
                spec_path: spec.clone(),
            }),
            payload: String::new(),
            evidence: String::new(),
        }]);
        let resolved = auto_resolve_deterministic_entries(&mut findings, &root)
            .expect("env-default resolver must succeed");
        assert_eq!(resolved.len(), 1);
        assert_eq!(resolved[0].new_value, "1");
    }

    #[test]
    fn auto_resolve_gdd_composition_reads_section_body() {
        let root = temp_dir("super-resolver-gdd");
        let gdd = root.join("RSOCIETY-GDD.md");
        fs::write(
            &gdd,
            "# Top\n\
## First Earned Cycle\n\
Composition: 60% relationship, 40% scaffold.\n\
Source: live runtime.\n\n\
## Next Section\n\
Other body.\n",
        )
        .unwrap();
        let mut findings = findings_with_entries(vec![OperatorQueueEntry {
            id: "OP-1".to_string(),
            title: "gdd section".to_string(),
            policy: OperatorPolicy::Deterministic,
            resolver_kind: Some(ResolverKind::GddComposition {
                section: "First Earned Cycle".to_string(),
            }),
            payload: String::new(),
            evidence: String::new(),
        }]);
        let resolved = auto_resolve_deterministic_entries(&mut findings, &root)
            .expect("gdd composition resolver must succeed");
        assert_eq!(resolved.len(), 1);
        assert!(resolved[0].new_value.contains("Composition: 60% relationship"));
        assert!(!resolved[0].new_value.contains("Other body"));
    }

    #[test]
    fn auto_resolve_skips_external_and_manual_policies() {
        let mut findings = findings_with_entries(vec![
            OperatorQueueEntry {
                id: "OP-EXT".to_string(),
                title: "external evidence".to_string(),
                policy: OperatorPolicy::External,
                resolver_kind: Some(ResolverKind::BaselineWalk {
                    current: 5,
                    floor: 0,
                }),
                payload: String::new(),
                evidence: String::new(),
            },
            OperatorQueueEntry {
                id: "OP-MAN".to_string(),
                title: "manual opt-out".to_string(),
                policy: OperatorPolicy::Manual,
                resolver_kind: Some(ResolverKind::BaselineWalk {
                    current: 5,
                    floor: 0,
                }),
                payload: String::new(),
                evidence: String::new(),
            },
        ]);
        let resolved = auto_resolve_deterministic_entries(&mut findings, Path::new("/tmp"))
            .expect("skip should still return Ok");
        assert!(resolved.is_empty());
        assert_eq!(findings.operator_queue.len(), 2);
        assert!(findings.auto_resolved.is_empty());
    }

    #[test]
    fn auto_resolve_records_failure_back_to_queue() {
        let root = temp_dir("super-resolver-fail");
        let mut findings = findings_with_entries(vec![OperatorQueueEntry {
            id: "OP-1".to_string(),
            title: "missing var".to_string(),
            policy: OperatorPolicy::Deterministic,
            resolver_kind: Some(ResolverKind::EnvDefault {
                var: "NOT_PRESENT".to_string(),
                spec_path: root.join("missing.md"),
            }),
            payload: String::new(),
            evidence: String::new(),
        }]);
        let resolved = auto_resolve_deterministic_entries(&mut findings, &root)
            .expect("resolver failure is non-fatal");
        assert!(resolved.is_empty());
        assert_eq!(findings.operator_queue.len(), 1);
        assert!(findings.operator_queue[0]
            .evidence
            .contains("auto-resolve failed"));
    }

    #[test]
    fn dispatch_classified_harvest_emits_queue_artifacts() {
        let root = temp_dir("super-dispatch");
        let cluster_gen = ClusterGroup {
            key: harvest_cluster::ClusterKey {
                path_ancestor: PathBuf::from("crates/foo/src/generated"),
                finding_class: "deepen".to_string(),
                signature_hash: "abc".to_string(),
            },
            seed: AuditFinding {
                dr_id: "DR-001".to_string(),
                title: "Schema drift".to_string(),
                cluster: "gen".to_string(),
                paths: vec!["crates/foo/src/generated/schema.rs".to_string()],
                class: crate::audit_everything::FindingClass::Deepen,
                complexity_hint: "single-row".to_string(),
                proof_found: String::new(),
                proof_missing: String::new(),
                risk: "med".to_string(),
                dedup_key: "key1".to_string(),
            },
            cluster_title: "Schema drift".to_string(),
            cluster_path: "crates/foo/src/generated/schema.rs".to_string(),
            dedup_keys: vec!["key1".to_string()],
            member_paths: vec!["crates/foo/src/generated/schema.rs".to_string()],
            member_count: 1,
        };
        let cluster_external = ClusterGroup {
            key: harvest_cluster::ClusterKey {
                path_ancestor: PathBuf::from("crates/pool/src"),
                finding_class: "deepen".to_string(),
                signature_hash: "def".to_string(),
            },
            seed: AuditFinding {
                dr_id: "DR-002".to_string(),
                title: "Pool acceptance lag".to_string(),
                cluster: "pool".to_string(),
                paths: vec!["crates/pool/src/accept.rs".to_string()],
                class: crate::audit_everything::FindingClass::Deepen,
                complexity_hint: "external-state".to_string(),
                proof_found: String::new(),
                proof_missing: "needs wall-clock pool acceptance".to_string(),
                risk: "med".to_string(),
                dedup_key: "key2".to_string(),
            },
            cluster_title: "Pool acceptance lag".to_string(),
            cluster_path: "crates/pool/src/accept.rs".to_string(),
            dedup_keys: vec!["key2".to_string()],
            member_paths: vec!["crates/pool/src/accept.rs".to_string()],
            member_count: 1,
        };
        let clusters = vec![cluster_gen, cluster_external];
        let outcome = dispatch_classified_harvest(&clusters, &root, "run-1").unwrap();
        assert!(outcome
            .artifacts
            .iter()
            .any(|p| p.ends_with("NEEDS-PLAN-QUEUE.json")));
        assert!(outcome
            .artifacts
            .iter()
            .any(|p| p.ends_with("OPERATOR-QUEUE.json")));
        assert!(outcome
            .deferred_paths
            .contains("crates/foo/src/generated/schema.rs"));
        assert!(outcome
            .deferred_paths
            .contains("crates/pool/src/accept.rs"));
    }
}
