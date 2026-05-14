// CEO functional review stage: run the corpus-review codex phase, validate
// `super-findings.json` + `SUPER-REPORT.md`, then surface the deterministic
// super-gate verdict to the operator log. Spliced into `super_command.rs`
// via `include!`.

async fn run_super_corpus_review(
    args: &SuperArgs,
    repo_root: &Path,
    planning_root: &Path,
    super_root: &Path,
) -> Result<()> {
    let prompt = build_super_corpus_review_prompt(repo_root, planning_root, super_root);
    run_super_codex_phase(
        repo_root,
        super_root,
        "super-corpus-review",
        &prompt,
        &args.model,
        &args.reasoning_effort,
        &args.codex_bin,
    )
    .await?;
    let findings_path = super_root.join(SUPER_FINDINGS_FILE);
    require_nonempty_file(&findings_path)?;
    let findings_text = fs::read_to_string(&findings_path)
        .with_context(|| format!("failed to read {}", findings_path.display()))?;
    let _: SuperFindings = serde_json::from_str(&findings_text)
        .with_context(|| format!("{} is not valid SuperFindings JSON", findings_path.display()))?;
    require_nonempty_file(&super_root.join(SUPER_REPORT_FILE))?;
    Ok(())
}

/// Run the deterministic gate over the freshly-resolved `super-findings.json`
/// and surface its verdict + blocker-bitrot delta to the operator log. This is
/// informational for v1 -- the execution-gate LLM run still decides Go/No-Go.
/// All errors are swallowed (logged via `eprintln!`) so a gate read failure
/// never blocks the super pipeline.
fn emit_super_gate_signals(repo_root: &Path, super_root: &Path) {
    let findings = match crate::super_gate::read_super_findings(super_root) {
        Ok(findings) => findings,
        Err(err) => {
            eprintln!("super_gate: could not read super-findings.json -- {err}");
            return;
        }
    };
    let outcome = crate::super_gate::severity_count_gate(&findings);
    match outcome.status {
        crate::super_gate::GateStatus::Go => {
            println!("super_gate: GO -- 0 high-severity blockers");
        }
        crate::super_gate::GateStatus::ConditionalGo => {
            println!(
                "super_gate: CONDITIONAL GO -- {} operator-queue entr{} still parked",
                outcome.deferred.len(),
                if outcome.deferred.len() == 1 { "y" } else { "ies" },
            );
        }
        crate::super_gate::GateStatus::NoGo => {
            eprintln!(
                "super_gate: NO-GO -- {} blocker(s) at severity:high",
                outcome.reasons.len(),
            );
            for reason in &outcome.reasons {
                eprintln!("  - {reason}");
            }
        }
    }

    if let Some(prev_run) = crate::super_gate::find_prior_super_run(repo_root, super_root) {
        match crate::super_gate::read_super_findings(&prev_run) {
            Ok(prev_findings) => {
                let bitrot = crate::super_gate::compute_blocker_bitrot(&prev_findings, &findings);
                if !bitrot.is_empty() {
                    eprintln!(
                        "super_gate: blocker bitrot detected ({} unchanged ID{} vs {})",
                        bitrot.len(),
                        if bitrot.len() == 1 { "" } else { "s" },
                        prev_run.display(),
                    );
                    for id in &bitrot {
                        eprintln!("  - {id}");
                    }
                }
            }
            Err(err) => {
                eprintln!(
                    "super_gate: prior run {} unreadable -- {err}",
                    prev_run.display(),
                );
            }
        }
    }
}

fn build_super_corpus_review_prompt(
    repo_root: &Path,
    planning_root: &Path,
    super_root: &Path,
) -> String {
    // Route this synthesis through the centrally-declared super tier so model
    // routing can be retuned in one place. The default tier is referenced for
    // documentation: the surrounding host owns model selection per CLI args.
    let _tier = PipelineStage::SuperSynthesis.default_tier();

    let role = format!(
        "You are the new CEO of this codebase running the `auto super` functional review war room.\n\
\n\
The normal `auto corpus` authoring and review passes have already produced `{planning_root}` for the repository at `{repo_root}`. The design perfection gate may also have written design/runtime artifacts under `{super_root}/design`. Treat those design artifacts as the first production-readiness input, not as a subordinate style appendix.\n\
\n\
Mission:\n\
- You inherited this codebase today.\n\
- You have 14 days to race it to production.\n\
- Compute and implementation capacity are not constraints; prioritization is about production leverage, risk, and dependency order.\n\
- Design/runtime integrity was perfected first. Now apply the same severity and precision across every functional lane.",
        repo_root = repo_root.display(),
        planning_root = planning_root.display(),
        super_root = super_root.display(),
    );

    let edit_boundary = format!(
        "- You may read the repository at `{repo_root}` and the planning corpus at `{planning_root}`.\n\
- You may read `{super_root}/design` and should preserve its runtime-first design/UI findings when they exist.\n\
- You may edit markdown files under `{planning_root}`.\n\
- You must write two artifacts under `{super_root}`: `{findings}` (canonical JSON, schema below) and `{report}` (human-readable narrative rendered from the same data).\n\
- Do not edit source code, root specs, root implementation plans, generated `gen-*` dirs, or skill definition directories.\n\
- Do not produce CEO-14-DAY-PLAN.md, FUNCTIONAL-REVIEWS.md, PRODUCTION-READINESS.md, RISK-REGISTER.md, QUALITY-GATES.md, SYSTEM-MAP.md, CROSS-REPO-MANIFEST.json, or a CODEBASE-BOOK/ tree under super; those legacy outputs are retired.",
        repo_root = repo_root.display(),
        planning_root = planning_root.display(),
        super_root = super_root.display(),
        findings = SUPER_FINDINGS_FILE,
        report = SUPER_REPORT_FILE,
    );

    let review_lanes = "Run these functional reviews and fold their disagreements into the JSON below:\n\
- CEO/Product: production definition, 10-star user outcome, non-goals, opportunity cost, scope discipline.\n\
- Design/Frontend: design-system clarity, modern UI quality, accessibility, AI-slop risk, runtime/UI drift.\n\
- Principal Engineer/Architecture: architecture seams, data flow, state, dependency order, maintainability.\n\
- Runtime/Engine: source-of-truth ownership, generated contracts, API/schema drift, state transitions, invariants.\n\
- Security/Trust: credentials, shell/YAML injection, secrets, dangerous flags, logs, authz, trust boundaries.\n\
- Reliability/Ops: idempotence, resume, partial failure, recovery, observability, receipts, operator handoff.\n\
- QA/Test Architect: missing regression tests, integration proof, false-positive verification, browser/runtime evidence.\n\
- Data/Contracts: migrations, compatibility, durable artifacts, schema ownership, backfill or rollback hazards.\n\
- Performance/Scale: hot paths, large repos, concurrency, resource cleanup, timeout behavior.\n\
- DX/Agent Workflow: first-run success, CLI help, errors, honest examples, setup friction, model/provider routing.\n\
- Release Manager: CI, install proof, versioning, rollback, release blockers, ship/no-ship criteria.";

    let findings_contract = format!(
        "`{findings}` is the single source of truth and MUST deserialize against this Rust schema (exact field names, snake_case):\n\
\n\
```json\n\
{{\n  \"run_id\": \"<string>\",\n  \"generated_at\": \"<RFC3339 timestamp>\",\n  \"readiness\": \"go\" | \"conditional_go\" | \"no_go\",\n  \"blockers\": [\n    {{\"id\": \"BLK-001\", \"title\": \"...\", \"owner_surface\": \"src/...\", \"severity\": \"high|med|low\", \"evidence\": \"path or command\", \"remediation_hint\": \"...\"}}\n  ],\n  \"risks\": [\n    {{\"id\": \"RSK-001\", \"title\": \"...\", \"likelihood\": \"high|med|low\", \"impact\": \"high|med|low\", \"mitigation\": \"...\"}}\n  ],\n  \"gates\": [\n    {{\"id\": \"GATE-001\", \"name\": \"pre-parallel\", \"status\": \"pass|fail|n/a\", \"evidence_paths\": [\"...\"]}}\n  ],\n  \"campaign_plan\": {{\n    \"horizon_days\": 14,\n    \"milestones\": [{{\"id\": \"M-1\", \"title\": \"...\", \"day\": 3, \"depends_on\": []}}]\n  }}\n}}\n```\n\
\n\
Rules:\n\
- The JSON is canonical. Every blocker, risk, gate, and milestone exists exactly once. Do not duplicate the same blocker as a risk + readiness paragraph + matrix row.\n\
- IDs must be stable, prefixed (`BLK-`, `RSK-`, `GATE-`, `M-`), and unique within their array.\n\
- `evidence` and `evidence_paths` must be concrete file paths, commands, or audit run IDs the next stage can re-run.\n\
- Severity / likelihood / impact values use the lowercase tokens shown above.\n\
\n\
`{report}` is the human-readable narrative rendered from that same JSON. It MUST cite each blocker / risk / gate by its JSON `id` so the two stay consistent. It also names the top non-blocking improvements, the not-doing list, how design was handled first, functional-lane risks, and any amendments made to `{planning_root}`.",
        findings = SUPER_FINDINGS_FILE,
        report = SUPER_REPORT_FILE,
        planning_root = planning_root.display(),
    );

    let amendment_clause = format!(
        "If the corpus under `{planning_root}` is missing production-readiness framing, amend it in place so the next `auto gen` pass produces release-oriented specs and executable plan tasks. Keep `genesis/` as corpus input, not a competing active control plane unless repository instructions explicitly say otherwise.",
        planning_root = planning_root.display(),
    );

    PromptSpec::new(role)
        .ethos(EthosPosture::EthosOnly)
        .edit_boundary(edit_boundary)
        .input("Review lanes", review_lanes)
        .output("Canonical findings + narrative", findings_contract)
        .evidence_item(format!(
            "`{}` deserializes against the SuperFindings schema; the host re-validates after this phase.",
            SUPER_FINDINGS_FILE,
        ))
        .evidence_item(format!(
            "`{}` references every blocker / risk / gate by its JSON `id`.",
            SUPER_REPORT_FILE,
        ))
        .evidence_item(
            "No legacy seven-file bundle is recreated; only the two canonical artifacts are written.",
        )
        .freeform_tail(amendment_clause)
        .render()
}
