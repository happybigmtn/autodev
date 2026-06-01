//! Automatic gbrain context capture for planning and generation prompts.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};

pub(crate) const GBRAIN_CONTEXT_FILENAME: &str = "GBRAIN-CONTEXT.md";

const COMMAND_TIMEOUT: Duration = Duration::from_secs(8);
const OUTPUT_LIMIT: usize = 8_000;

struct GbrainProbe {
    title: String,
    args: Vec<String>,
}

struct GbrainProbeOutput {
    title: String,
    command: String,
    status: String,
    stdout: String,
    stderr: String,
}

pub(crate) fn collect_gbrain_context(
    repo_root: &Path,
    destination_root: &Path,
    phase: &str,
    gbrain_bin: &Path,
) -> Result<PathBuf> {
    fs::create_dir_all(destination_root)
        .with_context(|| format!("failed to create {}", destination_root.display()))?;
    let repo_name = repo_root
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("repository");
    let probes = [
        GbrainProbe {
            title: "Current repo priority".to_string(),
            args: vec![
                "search".to_string(),
                format!("{repo_name} current priority"),
                "--limit".to_string(),
                "8".to_string(),
            ],
        },
        GbrainProbe {
            title: "Autodev corpus/gen memory".to_string(),
            args: vec![
                "search".to_string(),
                format!("{repo_name} auto corpus auto gen hermes"),
                "--limit".to_string(),
                "8".to_string(),
            ],
        },
        GbrainProbe {
            title: "Recent brain pages".to_string(),
            args: vec!["list".to_string(), "-n".to_string(), "12".to_string()],
        },
    ];
    let outputs = probes
        .into_iter()
        .map(|probe| run_probe(repo_root, gbrain_bin, probe))
        .collect::<Vec<_>>();
    let path = destination_root.join(GBRAIN_CONTEXT_FILENAME);
    fs::write(
        &path,
        render_context_markdown(repo_root, phase, gbrain_bin, &outputs),
    )
    .with_context(|| format!("failed to write {}", path.display()))?;
    Ok(path)
}

fn run_probe(repo_root: &Path, gbrain_bin: &Path, probe: GbrainProbe) -> GbrainProbeOutput {
    let command = display_command(gbrain_bin, &probe.args);
    match run_with_timeout(repo_root, gbrain_bin, &probe.args) {
        Ok((status, stdout, stderr)) => GbrainProbeOutput {
            title: probe.title,
            command,
            status,
            stdout: truncate(&stdout),
            stderr: truncate(&stderr),
        },
        Err(error) => GbrainProbeOutput {
            title: probe.title,
            command,
            status: format!("spawn failed: {error}"),
            stdout: String::new(),
            stderr: String::new(),
        },
    }
}

fn run_with_timeout(
    repo_root: &Path,
    gbrain_bin: &Path,
    args: &[String],
) -> Result<(String, String, String)> {
    let mut child = Command::new(gbrain_bin)
        .args(args)
        .current_dir(repo_root)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .with_context(|| format!("failed to spawn {}", gbrain_bin.display()))?;
    let started_at = Instant::now();
    loop {
        if child.try_wait()?.is_some() {
            let output = child.wait_with_output()?;
            return Ok((
                exit_status_label(output.status.code()),
                String::from_utf8_lossy(&output.stdout).to_string(),
                String::from_utf8_lossy(&output.stderr).to_string(),
            ));
        }
        if started_at.elapsed() >= COMMAND_TIMEOUT {
            let _ = child.kill();
            let output = child.wait_with_output()?;
            return Ok((
                format!("timed out after {}s", COMMAND_TIMEOUT.as_secs()),
                String::from_utf8_lossy(&output.stdout).to_string(),
                String::from_utf8_lossy(&output.stderr).to_string(),
            ));
        }
        thread::sleep(Duration::from_millis(50));
    }
}

fn exit_status_label(code: Option<i32>) -> String {
    match code {
        Some(0) => "exit 0".to_string(),
        Some(code) => format!("exit {code}"),
        None => "terminated by signal".to_string(),
    }
}

fn render_context_markdown(
    repo_root: &Path,
    phase: &str,
    gbrain_bin: &Path,
    outputs: &[GbrainProbeOutput],
) -> String {
    let mut markdown = format!(
        "# GBrain Context\n\nCollected automatically for `{phase}`.\n\n- Repo: `{}`\n- GBrain binary: `{}`\n\n## How To Use\n\n- Treat this as shared operator and project memory, not as authority for current code facts.\n- Verify filenames, APIs, commands, metrics, and implementation status against the checkout before planning.\n- Use this to preserve durable decisions, avoid repeating stale branch-memory work, and notice operator strategy that is not encoded in the repo yet.\n\n## Probe Results\n",
        repo_root.display(),
        gbrain_bin.display()
    );
    for output in outputs {
        markdown.push_str(&format!(
            "\n### {}\n\nCommand: `{}`\n\nStatus: `{}`\n\n",
            output.title, output.command, output.status
        ));
        if output.stdout.trim().is_empty() {
            markdown.push_str("_No stdout._\n");
        } else {
            markdown.push_str("Stdout:\n\n```text\n");
            markdown.push_str(output.stdout.trim_end());
            markdown.push_str("\n```\n");
        }
        if !output.stderr.trim().is_empty() && output.status != "exit 0" {
            markdown.push_str("\nStderr:\n\n```text\n");
            markdown.push_str(output.stderr.trim_end());
            markdown.push_str("\n```\n");
        }
    }
    markdown
}

fn display_command(gbrain_bin: &Path, args: &[String]) -> String {
    std::iter::once(gbrain_bin.display().to_string())
        .chain(args.iter().map(|arg| shell_display_arg(arg)))
        .collect::<Vec<_>>()
        .join(" ")
}

fn shell_display_arg(arg: &str) -> String {
    if arg.chars().any(char::is_whitespace) {
        format!("'{}'", arg.replace('\'', "'\\''"))
    } else {
        arg.to_string()
    }
}

fn truncate(text: &str) -> String {
    if text.len() <= OUTPUT_LIMIT {
        return text.to_string();
    }
    let mut end = OUTPUT_LIMIT;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    format!(
        "{}\n\n[truncated to {} bytes]",
        text[..end].trim_end(),
        OUTPUT_LIMIT
    )
}

#[cfg(test)]
mod tests {
    use super::{collect_gbrain_context, GBRAIN_CONTEXT_FILENAME};
    use std::fs;
    use std::path::Path;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_dir(label: &str) -> std::path::PathBuf {
        let suffix = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system time before unix epoch")
            .as_nanos();
        let path = std::env::temp_dir().join(format!("autodev-gbrain-{label}-{suffix}"));
        fs::create_dir_all(&path).expect("failed to create temp dir");
        path
    }

    #[test]
    fn unavailable_gbrain_still_writes_context_artifact() {
        let repo_root = temp_dir("repo");
        let destination = repo_root.join("genesis");
        let context_path = collect_gbrain_context(
            &repo_root,
            &destination,
            "auto corpus",
            Path::new("/definitely/not/a/gbrain/bin"),
        )
        .expect("collector should not fail on missing gbrain binary");

        assert_eq!(context_path, destination.join(GBRAIN_CONTEXT_FILENAME));
        let markdown = fs::read_to_string(context_path).expect("context artifact should exist");
        assert!(markdown.contains("# GBrain Context"));
        assert!(markdown.contains("spawn failed"));
        assert!(markdown.contains("shared operator and project memory"));
    }
}
