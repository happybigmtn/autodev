use std::fs;
use std::path::PathBuf;

#[test]
fn root_queue_mentions_generated_snapshots_only_as_evidence_or_promotion_inputs() {
    let plan = read_repo_file("IMPLEMENTATION_PLAN.md");

    assert!(
        plan.contains("`SAT-006` The planning corpus remains subordinate to root queue truth"),
        "SAT-006 closeout row should remain visible in the root plan"
    );
    assert!(
        plan.contains("references `gen-*` only as provenance or promotion evidence"),
        "SAT-006 should state the generated snapshot relationship"
    );

    let generated_reference_lines: Vec<_> = plan
        .lines()
        .filter(|line| line.contains("gen-20260430-184141") || line.contains("gen-*"))
        .collect();

    assert!(
        !generated_reference_lines.is_empty(),
        "root plan should retain generated snapshot provenance"
    );

    let disallowed_active_fields = [
        "Owns:",
        "Integration touchpoints:",
        "Dependencies:",
        "Estimated scope:",
        "Completion signal:",
    ];
    for line in &generated_reference_lines {
        assert!(
            !disallowed_active_fields
                .iter()
                .any(|field| line.trim_start().starts_with(field)),
            "generated snapshot reference appears in an active execution field: {line}"
        );
    }

    let promotion_or_evidence_contexts = [
        "Codebase evidence:",
        "Source of truth:",
        "Generated artifacts:",
        "Fixture boundary:",
        "Verification:",
        "Cross-surface tests:",
        "Evidence:",
    ];
    for line in &generated_reference_lines {
        assert!(
            promotion_or_evidence_contexts
                .iter()
                .any(|field| line.trim_start().starts_with(field)),
            "generated snapshot reference should be evidence or promotion context only: {line}"
        );
    }
}

#[test]
fn snapshot_only_decision_keeps_generated_outputs_subordinate_until_explicit_sync() {
    let decision = read_repo_file("docs/decisions/snapshot-only-generation.md");
    let readme = read_repo_file("README.md");

    assert!(
        decision.contains("Root sync is explicit through the existing `--sync-only` mode"),
        "decision should name the explicit promotion path"
    );
    assert!(
        decision
            .contains("Snapshot-only generation must not call `sync_verified_generation_outputs`"),
        "snapshot-only mode must not mutate root planning truth"
    );
    assert!(
        decision.contains(
            "Do not promote generated `gen-*` directories into an active planning control"
        ),
        "generated snapshots must stay subordinate until promoted"
    );

    assert!(
        readme.contains(
            "Use `auto gen --snapshot-only` when you want to inspect the generated `gen-*`"
        ),
        "README should present gen-* outputs as inspectable snapshots"
    );
    assert!(
        readme.contains("Use `auto gen --sync-only"),
        "README should point operators to sync-only for promotion"
    );
}

fn read_repo_file(relative_path: &str) -> String {
    fs::read_to_string(repo_root().join(relative_path))
        .unwrap_or_else(|err| panic!("failed to read {relative_path}: {err}"))
}

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}
