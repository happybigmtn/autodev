use std::path::Path;

use anyhow::{bail, Result};

use crate::util::{
    git_branch_exists, git_stdout, parse_origin_head_branch, KNOWN_PRIMARY_BRANCHES,
};

pub(crate) fn resolve_loop_branch(
    repo_root: &Path,
    requested_branch: Option<&str>,
    current_branch: &str,
) -> Result<String> {
    let origin_head = git_stdout(
        repo_root,
        [
            "symbolic-ref",
            "--quiet",
            "--short",
            "refs/remotes/origin/HEAD",
        ],
    )
    .ok();
    let available = KNOWN_PRIMARY_BRANCHES
        .into_iter()
        .filter(|candidate| git_branch_exists(repo_root, candidate))
        .collect::<Vec<_>>();
    pick_loop_branch(
        requested_branch,
        current_branch,
        origin_head.as_deref(),
        &available,
    )
}

fn pick_loop_branch(
    requested_branch: Option<&str>,
    current_branch: &str,
    origin_head: Option<&str>,
    available_primary_branches: &[&str],
) -> Result<String> {
    if let Some(branch) = requested_branch
        .map(str::trim)
        .filter(|branch| !branch.is_empty())
    {
        return Ok(branch.to_string());
    }

    if is_primary_branch_name(current_branch) {
        return Ok(current_branch.to_string());
    }

    if let Some(branch) = origin_head.and_then(parse_origin_head_branch) {
        return Ok(branch);
    }

    if let Some(branch) = KNOWN_PRIMARY_BRANCHES
        .into_iter()
        .find(|candidate| available_primary_branches.contains(candidate))
    {
        return Ok(branch.to_string());
    }

    bail!(
        "auto loop could not resolve the repo's primary branch; pass `--branch <name>` explicitly"
    );
}

fn is_primary_branch_name(branch: &str) -> bool {
    KNOWN_PRIMARY_BRANCHES.contains(&branch.trim())
}

#[cfg(test)]
mod tests {
    use super::pick_loop_branch;

    #[test]
    fn branch_picker_prefers_explicit_branch() {
        let branch =
            pick_loop_branch(Some("release"), "main", Some("origin/trunk"), &["trunk"]).unwrap();
        assert_eq!(branch, "release");
    }

    #[test]
    fn branch_picker_uses_origin_head_when_available() {
        let branch = pick_loop_branch(None, "feature/test", Some("origin/master"), &[]).unwrap();
        assert_eq!(branch, "master");
    }

    #[test]
    fn branch_picker_prefers_current_primary_branch_over_origin_head() {
        let branch =
            pick_loop_branch(None, "main", Some("origin/master"), &["main", "master"]).unwrap();
        assert_eq!(branch, "main");
    }

    #[test]
    fn branch_picker_falls_back_to_current_primary_branch() {
        let branch = pick_loop_branch(None, "trunk", None, &[]).unwrap();
        assert_eq!(branch, "trunk");
    }

    #[test]
    fn branch_picker_falls_back_to_known_available_branch() {
        let branch = pick_loop_branch(None, "feature/test", None, &["master"]).unwrap();
        assert_eq!(branch, "master");
    }
}
