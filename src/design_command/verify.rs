use std::fs;
use std::path::Path;

use anyhow::{bail, Context, Result};

use crate::design_command::prompt::DESIGN_ARTIFACTS;
use crate::verdict::{exact_terminal_verdict, terminal_verdict_is};

pub(crate) fn verify_design_artifacts(
    output_dir: &Path,
    operator_prompt: Option<&str>,
) -> Result<()> {
    for artifact in DESIGN_ARTIFACTS {
        let path = output_dir.join(artifact);
        if let Err(err) = require_nonempty_file(&path) {
            if operator_prompt
                .map(|p| prompt_bans_artifact(p, artifact))
                .unwrap_or(false)
            {
                eprintln!(
                    "design: artifact `{artifact}` was banned by operator directive; writing stub"
                );
                let stub = if artifact == "DESIGN-REPORT.md" {
                    // DESIGN-REPORT.md must contain a verdict header so downstream
                    // require_design_go can decide whether to proceed. When the
                    // operator forbids the file we conservatively stamp NO-GO so
                    // super pauses and the operator can review code commits made
                    // during the pass instead of waving through a missing verdict.
                    "# Banned by operator directive\n\n\
                     The operator focus prompt explicitly forbids creating `DESIGN-REPORT.md` during this design pass.\n\n\
                     Verdict: NO-GO\n\n\
                     Auto super stamped this NO-GO verdict because the operator forbade the canonical verdict file. \
                     Review the commits this pass produced and update `DESIGN-REPORT.md` manually with `Verdict: GO` \
                     (or invoke `auto super` with `--skip-design`) if you want downstream stages to proceed.\n"
                        .to_string()
                } else {
                    format!(
                        "# Banned by operator directive\n\nThe operator focus prompt explicitly forbids creating `{artifact}` during this design pass.\n\nSee `DESIGN-REPORT.md` for the canonical verdict and `git log` / the run's commit history for any code edits made during this pass.\n"
                    )
                };
                fs::write(&path, stub).with_context(|| {
                    format!("failed to write operator-ban stub to {}", path.display())
                })?;
            } else {
                return Err(err);
            }
        }
    }
    let report_path = output_dir.join("DESIGN-REPORT.md");
    let report = fs::read_to_string(&report_path)
        .with_context(|| format!("failed to read {}", report_path.display()))?;
    if exact_terminal_verdict(&report, &["Verdict: GO", "Verdict: NO-GO"])?.is_none() {
        bail!(
            "{} must contain `Verdict: GO` or `Verdict: NO-GO`",
            report_path.display()
        );
    }
    Ok(())
}

fn prompt_bans_artifact(prompt: &str, artifact: &str) -> bool {
    // Look at EVERY occurrence of the filename, not just the first. Operator
    // prompts commonly mention banned filenames twice: once in a prelude
    // explaining what NOT to repeat (e.g., "the prior design gate wrote ...
    // DESIGN-AUDIT.md ..."), then later in a "BANNED OUTPUTS" section. The
    // first occurrence has no ban keyword nearby, so checking only the first
    // produced a false negative.
    let mut search_from = 0usize;
    while let Some(rel) = prompt[search_from..].find(artifact) {
        let idx = search_from + rel;
        // Walk back to a UTF-8 char boundary to avoid panicking on the slice.
        let mut window_start = idx.saturating_sub(500);
        while window_start > 0 && !prompt.is_char_boundary(window_start) {
            window_start -= 1;
        }
        let window = prompt[window_start..idx].to_ascii_lowercase();
        if window.contains("no new ")
            || window.contains("banned")
            || window.contains("do not create")
            || window.contains("forbid")
            || window.contains("must not create")
        {
            return true;
        }
        search_from = idx + artifact.len();
    }
    false
}

pub(crate) fn require_design_go(output_dir: &Path) -> Result<()> {
    if design_report_is_go(output_dir)? {
        return Ok(());
    }
    let report_path = output_dir.join("DESIGN-REPORT.md");
    bail!(
        "design perfection gate did not approve downstream generation; expected `Verdict: GO` in {}",
        report_path.display()
    );
}

pub(crate) fn design_report_is_go(output_dir: &Path) -> Result<bool> {
    let report_path = output_dir.join("DESIGN-REPORT.md");
    let report = fs::read_to_string(&report_path)
        .with_context(|| format!("failed to read {}", report_path.display()))?;
    Ok(terminal_verdict_is(
        &report,
        "Verdict: GO",
        &["Verdict: GO", "Verdict: NO-GO"],
    ))
}

fn require_nonempty_file(path: &Path) -> Result<()> {
    let metadata = fs::metadata(path)
        .with_context(|| format!("required design artifact missing: {}", path.display()))?;
    if metadata.len() == 0 {
        bail!("required design artifact is empty: {}", path.display());
    }
    Ok(())
}
