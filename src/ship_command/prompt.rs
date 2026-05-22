pub(crate) const DEFAULT_SHIP_PROMPT_TEMPLATE: &str = r#"0a. Study `AGENTS.md` for repo-specific build, validation, staging, deployment, and local-run rules.
0b. Study `specs/*`, `IMPLEMENTATION_PLAN.md`, `COMPLETED.md`, `REVIEW.md`, `ARCHIVED.md`, `WORKLIST.md`, `LEARNINGS.md`, `QA.md`, `HEALTH.md`, `README.md`, `CHANGELOG.md`, and `VERSION` if they exist.
0c. Run a monolithic ship-prep pass after the mechanical release gate has passed or an operator bypass has been recorded in `SHIP.md`. You may use helper workflows or GitHub/deploy tools if they are available, but you must satisfy the shipping contract below even if those helpers are missing.

1. Your task is to prepare branch `{branch}` to ship against base branch `{base_branch}`.
   - Build a release checklist from the branch diff, the current QA and review state, and the repo's actual release surfaces.
   - Treat unresolved critical issues, broken validation, and stale documentation as shipping blockers until proven otherwise.
   - Do not invent release infrastructure that the repo does not have.

2. Use this shipping workflow end-to-end:
   - Confirm the current branch diff against `{base_branch}` and identify the blast radius of what is actually shipping.
   - If it is safe and necessary, bring the branch up to date with the latest remote base branch before continuing. If that sync becomes conflicted or ambiguous, stop and report the blocker truthfully.
   - Run the real validation commands required by this repo.
   - Review the shipping diff for release risk: structural regressions, accidental leftovers, docs drift, migration risk, security issues, performance regressions, accessibility regressions on user-facing surfaces, and missing verification.
   - If `VERSION` exists and the branch genuinely warrants a version update, update it truthfully.
   - If `CHANGELOG.md` exists, update only the relevant entry for what is actually shipping. Do not clobber unrelated history.
   - If README or other project docs drifted relative to what is shipping, sync them.
   - If `QA.md` or `HEALTH.md` is missing or obviously stale relative to the branch, run enough direct verification to ship truthfully instead of trusting stale reports.
   - If the repo uses feature flags, staged rollout controls, canaries, or safe-default rollout patterns, prefer deploy-off / release-on handling over immediate full exposure.

3. Maintain `SHIP.md` as the durable release report for this branch:
   - Record the branch, base branch, and the exact validations you ran.
   - Preserve any mechanical release-gate bypass reason already recorded by `auto ship`.
   - Record what changed for release bookkeeping: docs, changelog, version, or release notes.
   - Record shipping blockers, open follow-ups, and the final ship verdict.
   - Record the rollback path: what gets reverted, disabled, or rolled back first if this ship causes trouble.
   - Record the monitoring path: which metrics, logs, checks, dashboards, previews, or canary signals were actually available.
   - If a feature flag or staged rollout path exists, record the chosen rollout posture and any cleanup follow-up for that flag/control.
   - Append unresolved blockers or follow-up items to `WORKLIST.md` so they re-enter the active queue outside the release report.
   - If a PR exists or you create one, record the URL.
   - If you can perform preview, deploy, or post-push verification, record what you checked and what you observed.

4. Commit and push only truthful shipping increments:
   - Stay on branch `{branch}`.
   - Do not create or switch local branches.
   - Stage only the files relevant to shipping work plus `SHIP.md`, `CHANGELOG.md`, `VERSION`, docs, `WORKLIST.md`, `LEARNINGS.md`, `QA.md`, `HEALTH.md`, and `AGENTS.md` when they changed.
   - Commit with a message like `repo-name: ship prep`.
   - Push back to `origin/{branch}` after each successful commit-producing pass.
   - If `{branch}` is not `{base_branch}` and `gh` is available, create or refresh a PR targeting `{base_branch}`.
   - If `{branch}` already equals `{base_branch}`, skip PR creation and say so plainly in `SHIP.md`.

5. Post-push verification:
   - If the repo exposes preview URLs, deploy health checks, or a clear post-push verification path, run a lightweight verification pass and record the evidence.
   - If accessibility or performance checks are materially part of release confidence for a user-facing repo, record what you actually checked and what was not checked.
   - If deploy or canary verification is not realistically available, say so plainly instead of pretending the branch was production-verified.

6. Stop conditions:
   - If shipping blockers remain, do not fake readiness.
   - If validation is red and you cannot honestly fix it inside this pass, record the blocker in `SHIP.md` and `WORKLIST.md`, then stop.

99999. Important: shipping is a truth-telling workflow, not a ceremony workflow.
999999. Important: do not rewrite release history, changelog history, or version history casually.
9999991. Important: an operator bypass is not readiness; keep the bypass reason visible in `SHIP.md` until the missing evidence is replaced.
9999999. Important: prefer a blocked but honest ship report over a fake green release."#;

pub(crate) fn render_default_ship_prompt(branch: &str, base_branch: &str) -> String {
    DEFAULT_SHIP_PROMPT_TEMPLATE
        .replace("{branch}", branch)
        .replace("{base_branch}", base_branch)
}

#[cfg(test)]
mod tests {
    use super::render_default_ship_prompt;

    #[test]
    fn default_ship_prompt_includes_operational_release_controls() {
        let prompt = render_default_ship_prompt("main", "trunk");
        assert!(prompt.contains("mechanical release gate"));
        assert!(prompt.contains("bypass reason"));
        assert!(prompt.contains("rollback path"));
        assert!(prompt.contains("monitoring path"));
        assert!(prompt.contains("accessibility regressions"));
        assert!(prompt.contains("feature flags"));
        assert!(prompt.contains("branch `main`"));
        assert!(prompt.contains("base branch `trunk`"));
    }
}
