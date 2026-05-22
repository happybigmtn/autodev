use std::path::Path;

use anyhow::{bail, Result};

use crate::util::{
    git_branch_exists, git_stdout, parse_origin_head_branch, KNOWN_PRIMARY_BRANCHES,
};

pub(crate) fn resolve_base_branch(
    repo_root: &Path,
    requested_base_branch: Option<&str>,
    current_branch: &str,
) -> Result<String> {
    if let Some(branch) = requested_base_branch
        .map(str::trim)
        .filter(|branch| !branch.is_empty())
    {
        return Ok(branch.to_string());
    }

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
    if let Some(branch) = origin_head.and_then(|value| parse_origin_head_branch(&value)) {
        return Ok(branch);
    }

    if KNOWN_PRIMARY_BRANCHES.contains(&current_branch) {
        return Ok(current_branch.to_string());
    }

    if let Some(branch) = KNOWN_PRIMARY_BRANCHES
        .into_iter()
        .find(|candidate| git_branch_exists(repo_root, candidate))
    {
        return Ok(branch.to_string());
    }

    bail!(
        "auto ship could not resolve the repo's base branch; pass `--base-branch <name>` explicitly"
    );
}

#[cfg(test)]
mod tests {
    use std::process::Command;

    use super::resolve_base_branch;
    use crate::ship_command::testkit::{init_git_repo, test_dir};
    use crate::util::git_stdout;

    #[test]
    fn resolve_base_branch_prefers_current_branch_when_it_is_primary() {
        let repo = test_dir("base-branch-prefers-current");
        init_git_repo(&repo);

        // Create both main and master branches, checkout main
        Command::new("git")
            .arg("-C")
            .arg(&repo)
            .args(["branch", "master"])
            .output()
            .expect("git branch master failed");
        Command::new("git")
            .arg("-C")
            .arg(&repo)
            .args(["checkout", "-b", "main"])
            .output()
            .expect("git checkout main failed");

        let current = git_stdout(&repo, ["branch", "--show-current"])
            .expect("git branch --show-current failed");
        assert_eq!(current.trim(), "main");

        let base = resolve_base_branch(&repo, None, "main").expect("resolve_base_branch failed");
        assert_eq!(
            base, "main",
            "expected main when currently on main, got {base}"
        );
    }

    #[test]
    fn resolve_base_branch_falls_back_to_other_primary_when_current_is_not_primary() {
        let repo = test_dir("base-branch-fallback");
        init_git_repo(&repo);

        // Create a feature branch from master, leaving master as the only primary
        Command::new("git")
            .arg("-C")
            .arg(&repo)
            .args(["checkout", "-b", "feature"])
            .output()
            .expect("git checkout feature failed");

        let base = resolve_base_branch(&repo, None, "feature").expect("resolve_base_branch failed");
        assert_eq!(
            base, "master",
            "expected master when on feature branch, got {base}"
        );
    }
}
