use anyhow::{bail, Result};

/// Render the prompt-side instruction that tells a worker which terminal verdict
/// lines are acceptable. The emitted text is paired with the parser
/// (`exact_terminal_verdict`) so prompt language and parser cannot drift apart.
///
/// Use this from every prompt builder that asks for a `Verdict:` line. The
/// historical pattern was an inline string of "A line exactly `Verdict: GO` or
/// `Verdict: NO-GO`" copied across 45+ sites; small drifts (parens, casing,
/// trailing punctuation) led to false negatives.
pub(crate) fn verdict_footer(allowed: &[&str]) -> String {
    let quoted: Vec<String> = allowed.iter().map(|line| format!("`{line}`")).collect();
    let alternatives = match quoted.as_slice() {
        [] => "`Verdict: GO` or `Verdict: NO-GO`".to_string(),
        [single] => single.clone(),
        [a, b] => format!("{a} or {b}"),
        many => {
            let (last, head) = many.split_last().expect("non-empty");
            format!("{}, or {last}", head.join(", "))
        }
    };
    format!(
        "# Terminal verdict\n\nEnd the response with a line that is exactly one of: {alternatives}. \
         Do not add punctuation, parentheses, or qualifiers to that line; the host parses it byte-for-byte."
    )
}

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
    use super::{exact_terminal_verdict, terminal_verdict_is, verdict_footer};

    #[test]
    fn verdict_footer_includes_each_alternative_quoted() {
        let footer = verdict_footer(&["Verdict: GO", "Verdict: NO-GO"]);
        assert!(footer.contains("`Verdict: GO`"));
        assert!(footer.contains("`Verdict: NO-GO`"));
        assert!(footer.contains("byte-for-byte"));
    }

    #[test]
    fn verdict_footer_round_trips_with_parser() {
        // The strings the footer instructs the worker to emit must round-trip
        // through exact_terminal_verdict without flagging them invalid. This
        // guards the prompt language and the parser from drifting apart.
        let allowed = ["Verdict: PASS", "Verdict: NO-GO"];
        let footer = verdict_footer(&allowed);
        for verdict in &allowed {
            let sample = format!("Summary text.\n\n{verdict}\n");
            let parsed = exact_terminal_verdict(&sample, &allowed)
                .expect("parse")
                .expect("verdict present");
            assert_eq!(parsed, *verdict, "footer={footer}");
        }
    }

    #[test]
    fn verdict_footer_three_alternatives_uses_oxford_comma() {
        let footer = verdict_footer(&["Verdict: GO", "Verdict: SOFT-GO", "Verdict: NO-GO"]);
        assert!(footer.contains("`Verdict: GO`, `Verdict: SOFT-GO`, or `Verdict: NO-GO`"));
    }

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
