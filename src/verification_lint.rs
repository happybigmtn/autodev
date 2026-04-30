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
    if argv.iter().any(|arg| arg == "--lib") {
        bail!(
            "task `{task_id}` `{field}` uses stale `cargo test --lib` verification command `{command}` for this bin-only crate; use an exact test filter through the default bin target"
        );
    }

    let filters = cargo_test_filter_tokens(argv);
    if filters.len() > 1 {
        bail!(
            "task `{task_id}` `{field}` uses multi-filter cargo test verification command `{command}`; split it into one runnable cargo test command per filter"
        );
    }

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
        if token == "--" || token == "&&" || token == ";" || token == "||" {
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

fn operand_looks_like_directory(operand: &str) -> bool {
    operand.ends_with('/')
        || (!operand.contains('*')
            && !operand.contains('?')
            && !operand.contains('.')
            && !operand.starts_with('$')
            && !operand.starts_with('<')
            && !operand.starts_with('>'))
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
