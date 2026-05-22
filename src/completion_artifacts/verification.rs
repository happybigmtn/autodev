//! Verification-step parsing: executable commands and narrative guidance.

use crate::task_parser::parse_tasks as parse_shared_tasks;

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct VerificationPlan {
    pub(crate) steps: Vec<String>,
    pub(crate) executable_commands: Vec<String>,
    pub(crate) narrative_guidance: Vec<String>,
}

pub(crate) fn verification_plan(task_markdown: &str) -> VerificationPlan {
    let Some(body) = parse_shared_tasks(task_markdown)
        .into_iter()
        .next()
        .and_then(|task| task.verification_text)
    else {
        return VerificationPlan::default();
    };

    let steps = body
        .lines()
        .map(strip_list_bullet)
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(str::to_string)
        .collect::<Vec<_>>();
    let executable_commands = steps
        .iter()
        .flat_map(|step| executable_commands_from_verification_step(step))
        .collect::<Vec<_>>();
    let narrative_guidance = steps
        .iter()
        .filter(|step| executable_commands_from_verification_step(step).is_empty())
        .cloned()
        .collect::<Vec<_>>();
    VerificationPlan {
        steps,
        executable_commands,
        narrative_guidance,
    }
}

pub(crate) fn verification_step_looks_external(step: &str) -> bool {
    let step = step.to_ascii_lowercase();
    [
        "http://",
        "https://",
        "ssh ",
        "kubectl",
        "hcloud",
        "github ui",
        "grafana import",
        "inspect live",
        "reference host",
        "loom host",
        "staging alertmanager",
        "external dogfood",
        "deploy_house.sh deploy",
    ]
    .into_iter()
    .any(|marker| step.contains(marker))
}

fn executable_commands_from_verification_step(step: &str) -> Vec<String> {
    let step = step.trim();
    if step.is_empty() {
        return Vec::new();
    }

    let backtick_commands = backtick_fragments(step)
        .into_iter()
        .filter(|candidate| looks_like_executable_command(candidate))
        .collect::<Vec<_>>();
    if !backtick_commands.is_empty() {
        return backtick_commands;
    }

    let candidate = truncate_verification_narrative(step);
    if looks_like_executable_command(candidate) {
        vec![candidate.to_string()]
    } else {
        Vec::new()
    }
}

pub(crate) fn backtick_fragments(line: &str) -> Vec<String> {
    let mut fragments = Vec::new();
    let mut rest = line;
    while let Some(start) = rest.find('`') {
        rest = &rest[start + 1..];
        let Some(end) = rest.find('`') else {
            break;
        };
        let candidate = rest[..end].trim();
        if !candidate.is_empty() {
            fragments.push(candidate.to_string());
        }
        rest = &rest[end + 1..];
    }
    fragments
}

fn truncate_verification_narrative(step: &str) -> &str {
    let narrative_markers = [
        "; same command",
        "; production",
        "; glossary",
        "; privacy audit",
        " exits ",
        " returns ",
        " starts ",
        " succeeds ",
        " fails ",
        " within ",
        " without ",
    ];
    let lower = step.to_ascii_lowercase();
    let cut = narrative_markers
        .iter()
        .filter_map(|marker| lower.find(marker))
        .min()
        .unwrap_or(step.len());
    step[..cut].trim()
}

fn looks_like_executable_command(candidate: &str) -> bool {
    let candidate = candidate.trim();
    if candidate.is_empty() || candidate.starts_with('-') || candidate.contains('→') {
        return false;
    }

    let first = candidate.split_whitespace().next().unwrap_or_default();
    if first.is_empty() {
        return false;
    }

    if is_env_assignment(first) {
        return candidate.split_whitespace().nth(1).is_some();
    }

    let shell_prefixes = [
        "./", "cargo", "bash", "sh", "python", "python3", "node", "pnpm", "npm", "yarn", "rg",
        "grep", "curl", "ssh", "docker", "kubectl", "git", "make", "just", "uv", "go", "pytest",
        "scripts/",
    ];
    shell_prefixes
        .iter()
        .any(|prefix| first == *prefix || first.starts_with(prefix))
        && candidate.split_whitespace().nth(1).is_some()
}

fn is_env_assignment(token: &str) -> bool {
    let Some((name, value)) = token.split_once('=') else {
        return false;
    };
    !value.is_empty()
        || token.ends_with('=')
            && !name.is_empty()
            && name
                .chars()
                .all(|ch| ch.is_ascii_uppercase() || ch.is_ascii_digit() || ch == '_')
}

fn strip_list_bullet(line: &str) -> &str {
    let trimmed = line.trim_start();
    for bullet in ["- ", "* ", "+ "] {
        if let Some(rest) = trimmed.strip_prefix(bullet) {
            return rest;
        }
    }
    trimmed
}

#[cfg(test)]
mod tests {
    use super::verification_plan;

    #[test]
    fn verification_plan_preserves_narrative_without_treating_it_as_shell() {
        let plan = verification_plan(
            "- [~] `TASK-5` Dashboard task\nVerification:\n  - Grafana import on reference host succeeds; glossary cross-links resolve.\nRequired tests: none\nDependencies: none\n",
        );
        assert!(plan.executable_commands.is_empty());
        assert_eq!(plan.narrative_guidance.len(), 1);
    }

    #[test]
    fn verification_plan_extracts_backtick_commands_without_bare_flags() {
        let plan = verification_plan(
            "- [~] `TASK-6` Fail fast\nVerification:\n  - `BITINO_HOUSE_SESSION_SECRET= cargo run -p bitino-house` exits non-zero; same command with `--dev` starts + warns; production container with `--dev` fails CI.\nRequired tests: none\nDependencies: none\n",
        );
        assert_eq!(
            plan.executable_commands,
            vec!["BITINO_HOUSE_SESSION_SECRET= cargo run -p bitino-house".to_string()]
        );
        assert!(plan.narrative_guidance.is_empty());
    }

    #[test]
    fn verification_plan_stops_at_dependencies_before_completion_notes() {
        let plan = verification_plan(
            "- [x] `TASK-9` Completed\nVerification: `rg -n 'Thing' src`\nDependencies: none\nEstimated scope: S\n- Completed 2026-04-21: added proof.\n- Verification 2026-04-21: `cargo test -p demo hidden`\n",
        );
        assert_eq!(
            plan.executable_commands,
            vec!["rg -n 'Thing' src".to_string()]
        );
    }
}
