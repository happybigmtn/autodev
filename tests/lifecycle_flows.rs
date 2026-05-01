use std::fs;
use std::path::PathBuf;

#[test]
fn lifecycle_fixture_rejects_unlabeled_or_unauthorized_lifecycle_claims() {
    let readme = read_repo_file("README.md");
    let schema = read_repo_file("docs/verification-receipt-schema.md");

    for label in [
        "Evidence Class: executable",
        "Evidence Class: external",
        "Evidence Class: operator-waiver",
        "Evidence Class: archive",
    ] {
        assert!(
            schema.contains(label),
            "receipt schema should define `{label}`"
        );
    }
    assert!(
        readme.contains("Report-only commands may write only their named report"),
        "README should state report-only lifecycle write boundary"
    );
}

fn read_repo_file(relative_path: &str) -> String {
    fs::read_to_string(repo_root().join(relative_path))
        .unwrap_or_else(|err| panic!("failed to read {relative_path}: {err}"))
}

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}
