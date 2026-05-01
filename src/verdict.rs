use anyhow::{bail, Result};

pub(crate) fn exact_terminal_verdict(text: &str, allowed: &[&str]) -> Result<Option<String>> {
    let verdicts = text
        .lines()
        .filter_map(|line| {
            let trimmed = line.trim();
            allowed
                .iter()
                .find(|allowed| trimmed.eq_ignore_ascii_case(allowed))
                .map(|allowed| (*allowed).to_string())
                .or_else(|| {
                    trimmed
                        .starts_with("Verdict:")
                        .then(|| format!("invalid terminal verdict line `{trimmed}`"))
                })
        })
        .collect::<Vec<_>>();
    if verdicts.is_empty() {
        return Ok(None);
    }
    if verdicts.len() > 1 {
        bail!(
            "expected exactly one terminal verdict line, found {}",
            verdicts.len()
        );
    }
    let verdict = &verdicts[0];
    if verdict.starts_with("invalid terminal verdict line") {
        bail!("{verdict}");
    }
    Ok(Some(verdict.clone()))
}

pub(crate) fn terminal_verdict_is(text: &str, expected: &str, allowed: &[&str]) -> bool {
    exact_terminal_verdict(text, allowed)
        .ok()
        .flatten()
        .is_some_and(|verdict| verdict.eq_ignore_ascii_case(expected))
}

#[cfg(test)]
mod tests {
    use super::{exact_terminal_verdict, terminal_verdict_is};

    #[test]
    fn exact_terminal_verdict_rejects_mixed_verdicts() {
        let text = "Verdict: GO\n\nLater:\nVerdict: NO-GO\n";
        let err = exact_terminal_verdict(text, &["Verdict: GO", "Verdict: NO-GO"])
            .expect_err("mixed verdicts rejected");
        assert!(format!("{err:#}").contains("exactly one"));
    }

    #[test]
    fn terminal_verdict_is_requires_exact_single_line() {
        assert!(terminal_verdict_is(
            "Summary\n\nVerdict: PASS\n",
            "Verdict: PASS",
            &["Verdict: PASS", "Verdict: NO-GO"],
        ));
        assert!(!terminal_verdict_is(
            "Verdict: PASS-ish\n",
            "Verdict: PASS",
            &["Verdict: PASS", "Verdict: NO-GO"],
        ));
    }
}
