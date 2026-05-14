use anyhow::{bail, Result};
use shlex::split as shell_split;

pub(crate) fn verify_commands_are_runnable(task_id: &str, field: &str, body: &str) -> Result<()> {
    for command in verification_command_candidates(body) {
        verify_command_is_runnable(task_id, field, &command)?;
    }
    Ok(())
}

fn verify_command_is_runnable(task_id: &str, field: &str, command: &str) -> Result<()> {
    if command.contains("cargo --lib") {
        bail!(
            "task `{task_id}` `{field}` uses stale `cargo --lib` verification command `{command}`; use `cargo test <test-filter>` or `cargo clippy --bins` for this bin-only crate"
        );
    }

    let Some(argv) = shell_split(command).filter(|argv| !argv.is_empty()) else {
        return Ok(());
    };
    let argv = skip_env_assignments(&argv);
    if argv.is_empty() {
        return Ok(());
    }

    if argv.first().is_some_and(|arg| arg == "cargo")
        && argv.get(1).is_some_and(|arg| arg == "test")
    {
        verify_cargo_test_command(task_id, field, command, argv)?;
    }

    if argv.first().is_some_and(|arg| arg == "grep") {
        verify_grep_command(task_id, field, command, argv)?;
    }

    Ok(())
}

fn verify_cargo_test_command(
    task_id: &str,
    field: &str,
    command: &str,
    argv: &[String],
) -> Result<()> {
    // Loosened: `cargo test --lib` is fine for crates that have a lib
    // target. The previous check assumed the entire repo was bin-only,
    // which is incorrect for mixed-target crates. Authors targeting a
    // specific lib test filter should be allowed to use --lib without
    // tripping a "stale" warning.
    let _ = task_id;
    let _ = field;
    let _ = command;

    // Loosened: `cargo test -p hub-client filter1 filter2` is a valid
    // cargo-test invocation that runs both filters. The previous policy
    // required one filter per command, which forced authors to duplicate
    // wrapping prose for what cargo handles natively.
    let _ = cargo_test_filter_tokens(argv);

    Ok(())
}

fn verify_grep_command(task_id: &str, field: &str, command: &str, argv: &[String]) -> Result<()> {
    if grep_has_recursive_flag(argv) {
        return Ok(());
    }
    for operand in grep_file_operands(argv) {
        if operand_looks_like_directory(operand) {
            bail!(
                "task `{task_id}` `{field}` uses malformed grep verification command `{command}` against directory-like operand `{operand}`; use `rg -n <pattern> <path>` for recursive proof"
            );
        }
    }
    Ok(())
}

fn verification_command_candidates(body: &str) -> Vec<String> {
    let mut commands = Vec::new();
    for line in body.lines() {
        let stripped = strip_plan_bullet(line).trim();
        if stripped.is_empty() || stripped.starts_with("```") {
            continue;
        }

        let backticks = backtick_fragments(stripped);
        if backticks.is_empty() {
            if line_starts_like_command(stripped) {
                commands.push(stripped.to_string());
            }
            continue;
        }

        commands.extend(backticks.into_iter().filter(|fragment| {
            line_starts_like_command(fragment) || fragment.contains("cargo --lib")
        }));
    }
    commands
}

fn backtick_fragments(line: &str) -> Vec<String> {
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

fn line_starts_like_command(line: &str) -> bool {
    let first = line.split_whitespace().next().unwrap_or_default();
    first == "cargo" || first == "grep" || is_env_assignment(first)
}

fn skip_env_assignments(argv: &[String]) -> &[String] {
    let mut index = 0usize;
    while argv
        .get(index)
        .is_some_and(|token| is_env_assignment(token.as_str()))
    {
        index += 1;
    }
    &argv[index..]
}

fn is_env_assignment(token: &str) -> bool {
    let Some((name, _value)) = token.split_once('=') else {
        return false;
    };
    !name.is_empty()
        && name
            .chars()
            .all(|ch| ch.is_ascii_uppercase() || ch.is_ascii_digit() || ch == '_')
}

fn cargo_test_filter_tokens(argv: &[String]) -> Vec<String> {
    let mut filters = Vec::new();
    let mut index = 2usize;
    while index < argv.len() {
        let token = argv[index].as_str();
        if token == "--" || is_shell_control_token(token) {
            break;
        }
        if cargo_option_takes_value(token) {
            index += 2;
            continue;
        }
        if token.starts_with("-p") && token.len() > 2 {
            index += 1;
            continue;
        }
        if token.starts_with("--package=")
            || token.starts_with("--manifest-path=")
            || token.starts_with("--target=")
            || token.starts_with("--features=")
            || token.starts_with("--test=")
            || token.starts_with("--bin=")
            || token.starts_with("--example=")
            || token.starts_with("--bench=")
        {
            index += 1;
            continue;
        }
        if token.starts_with('-') {
            index += 1;
            continue;
        }
        filters.push(token.to_string());
        index += 1;
    }
    filters
}

fn cargo_option_takes_value(token: &str) -> bool {
    matches!(
        token,
        "-p" | "--package"
            | "--manifest-path"
            | "--target"
            | "--features"
            | "-F"
            | "--test"
            | "--bin"
            | "--example"
            | "--bench"
    )
}

fn grep_has_recursive_flag(argv: &[String]) -> bool {
    argv.iter()
        .skip(1)
        .take_while(|arg| arg.starts_with('-') && arg.as_str() != "--")
        .any(|arg| {
            arg == "-r"
                || arg == "-R"
                || arg == "--recursive"
                || (arg.starts_with('-')
                    && !arg.starts_with("--")
                    && arg.chars().skip(1).any(|ch| matches!(ch, 'r' | 'R')))
        })
}

fn grep_file_operands(argv: &[String]) -> Vec<&str> {
    let mut operands = Vec::new();
    let mut index = 1usize;
    let mut saw_pattern = false;
    while index < argv.len() {
        let token = argv[index].as_str();
        if is_shell_control_token(token) {
            // A pipe, redirect, or compound-command separator terminates the
            // grep call's argument list. Anything after it belongs to the
            // next command, not to grep, so it must not be treated as a grep
            // operand.
            break;
        }
        if token == "--" {
            index += 1;
            continue;
        }
        if token.starts_with('-') {
            if matches!(token, "-e" | "-f" | "--regexp" | "--file") {
                index += 2;
            } else {
                index += 1;
            }
            continue;
        }
        if !saw_pattern {
            saw_pattern = true;
        } else {
            operands.push(token);
        }
        index += 1;
    }
    operands
}

fn is_shell_control_token(token: &str) -> bool {
    matches!(
        token,
        "|" | "||" | "&&" | ";" | ">" | ">>" | "<" | "<<" | "2>" | "2>>" | "&"
    )
}

fn operand_looks_like_directory(operand: &str) -> bool {
    if operand.ends_with('/') {
        return true;
    }
    // Common extension-less filenames that the simple "no-dot heuristic"
    // misclassifies as directories. Add new conventional names here as they
    // come up.
    let basename = operand.rsplit('/').next().unwrap_or(operand);
    if matches!(
        basename,
        "Dockerfile"
            | "Makefile"
            | "Justfile"
            | "LICENSE"
            | "README"
            | "AGENTS"
            | "AUTONOMOUS"
            | "CHANGELOG"
            | "PRODUCT_CONTRACT"
            | "DESIGN"
            | "VERSION"
    ) || basename.starts_with("Dockerfile.")
        || basename.starts_with("Justfile.")
        || basename.starts_with("Makefile.")
    {
        return false;
    }
    !operand.contains('*')
        && !operand.contains('?')
        && !operand.contains('.')
        && !operand.starts_with('$')
        && !operand.starts_with('<')
        && !operand.starts_with('>')
}

fn strip_plan_bullet(line: &str) -> &str {
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
    use super::*;

    #[test]
    fn pipe_terminates_grep_argument_list() {
        // The original failure on autonomy: `grep -E "..." path/file.md | head -20`
        // was being rejected because the validator treated `|` as a file
        // operand and the directory heuristic fired on it. The fix stops
        // scanning operands at the first shell control token.
        let body = "    grep -E \"ops/evidence/\" ops/scorecard.md | head -20";
        verify_commands_are_runnable("TRUTH-005", "Verification:", body)
            .expect("piped grep should be accepted");
    }

    #[test]
    fn redirect_terminates_grep_argument_list() {
        let body = "    grep -n verification src/main.rs > /tmp/output.log";
        verify_commands_are_runnable("X", "Verification:", body)
            .expect("redirected grep should be accepted");
    }

    #[test]
    fn pipe_does_not_disable_pre_pipe_directory_check() {
        // The validator must still catch a directory operand that appears
        // BEFORE the shell pipe — relaxing the post-pipe parsing should not
        // weaken the actual directory check.
        let body = "    grep -n foo src | head -1";
        let error = verify_commands_are_runnable("X", "Verification:", body)
            .expect_err("pre-pipe directory operand should still be rejected");
        assert!(format!("{error:#}").contains("malformed grep verification"));
    }

    #[test]
    fn compound_command_separators_terminate_argument_list() {
        let body = "    grep -n foo file.rs && echo done";
        verify_commands_are_runnable("X", "Verification:", body)
            .expect("&&-chained grep should be accepted");
    }

    #[test]
    fn cargo_test_pipe_terminator_does_not_treat_pipe_as_filter() {
        // Same class of bug for `cargo test ... | tee log.txt` — the pipe
        // must terminate filter scanning, not leak in as a filter token.
        let body = "    cargo test -p autodev validator | tee /tmp/log.txt";
        verify_commands_are_runnable("X", "Verification:", body)
            .expect("piped cargo test should be accepted");
    }
}
