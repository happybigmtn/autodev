use std::fmt::Write as _;

use console::Style;
use serde_json::Value;

use crate::util::clip_line_for_display;

#[derive(Default)]
pub(super) struct UsageSummary {
    pub(super) input_tokens: u64,
    pub(super) cached_input_tokens: u64,
    pub(super) output_tokens: u64,
}

pub(super) fn push_plain_line(out: &mut String, line: &str) {
    let sanitized = sanitize_terminal_text(line);
    let _ = writeln!(out, "{sanitized}");
}

pub(super) fn push_styled_line(out: &mut String, style: &Style, line: impl AsRef<str>) {
    let sanitized = sanitize_terminal_text(line.as_ref());
    let _ = writeln!(out, "{}", style.apply_to(sanitized));
}

pub(super) fn write_block(
    out: &mut String,
    prefix: &str,
    text: Option<String>,
    style: &Style,
    limit: usize,
) {
    let Some(text) = text else {
        return;
    };
    let sanitized = sanitize_terminal_text(&text);
    let lines = sanitized
        .lines()
        .map(str::trim_end)
        .filter(|line| !line.trim().is_empty())
        .collect::<Vec<_>>();
    for line in lines.iter().take(limit) {
        let clipped = if line.chars().count() > 140 {
            format!("{}...", clip_line_for_display(line, 137))
        } else {
            (*line).to_string()
        };
        push_styled_line(out, style, format!("{prefix}{clipped}"));
    }
    if lines.len() > limit {
        push_styled_line(
            out,
            &Style::new().dim(),
            format!("{prefix}... +{} more lines", lines.len() - limit),
        );
    }
}

pub(super) fn json_string(value: &Value, key: &str) -> Option<String> {
    value
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .map(str::to_string)
}

pub(super) fn compact_json(value: &Value) -> Option<String> {
    if value.is_null() {
        return None;
    }
    serde_json::to_string(value)
        .ok()
        .map(|text| text.trim().to_string())
        .filter(|text| !text.is_empty() && text != "null")
}

pub(super) fn extract_content_text(content: &[Value]) -> String {
    let mut parts = Vec::new();
    for item in content {
        if let Some(text) = item
            .get("text")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|text| !text.is_empty())
        {
            parts.push(text.to_string());
            continue;
        }
        if let Some(text) = item.as_str().map(str::trim).filter(|text| !text.is_empty()) {
            parts.push(text.to_string());
            continue;
        }
        if let Some(summary) = compact_json(item) {
            parts.push(summary);
        }
    }
    parts.join("\n")
}

pub(super) fn sanitize_terminal_text(input: &str) -> String {
    let mut chars = input.chars().peekable();
    let mut out = String::with_capacity(input.len());
    while let Some(ch) = chars.next() {
        match ch {
            '\u{1b}' => skip_escape_sequence(&mut chars),
            '\u{009b}' => skip_csi_sequence(&mut chars),
            '\u{009d}' => skip_osc_sequence(&mut chars),
            '\u{08}' => pop_last_inline_char(&mut out),
            '\r' => {
                if chars.peek() != Some(&'\n') && !out.ends_with('\n') {
                    out.push('\n');
                }
            }
            '\n' | '\t' => out.push(ch),
            '\u{00}'..='\u{1f}' | '\u{7f}'..='\u{9f}' => {}
            _ => out.push(ch),
        }
    }
    out
}

fn skip_escape_sequence<I>(chars: &mut std::iter::Peekable<I>)
where
    I: Iterator<Item = char>,
{
    match chars.peek().copied() {
        Some('[') => {
            chars.next();
            skip_csi_sequence(chars);
        }
        Some(']') => {
            chars.next();
            skip_osc_sequence(chars);
        }
        Some('P' | 'X' | '^' | '_') => {
            chars.next();
            skip_st_sequence(chars);
        }
        Some(_) => {
            chars.next();
        }
        None => {}
    }
}

fn skip_csi_sequence<I>(chars: &mut std::iter::Peekable<I>)
where
    I: Iterator<Item = char>,
{
    for ch in chars.by_ref() {
        if ('@'..='~').contains(&ch) {
            break;
        }
    }
}

fn skip_osc_sequence<I>(chars: &mut std::iter::Peekable<I>)
where
    I: Iterator<Item = char>,
{
    while let Some(ch) = chars.next() {
        match ch {
            '\u{07}' => break,
            '\u{1b}' if chars.peek() == Some(&'\\') => {
                chars.next();
                break;
            }
            _ => {}
        }
    }
}

fn skip_st_sequence<I>(chars: &mut std::iter::Peekable<I>)
where
    I: Iterator<Item = char>,
{
    while let Some(ch) = chars.next() {
        if ch == '\u{1b}' && chars.peek() == Some(&'\\') {
            chars.next();
            break;
        }
    }
}

fn pop_last_inline_char(out: &mut String) {
    if out.ends_with('\n') {
        return;
    }
    out.pop();
}

#[cfg(test)]
mod tests {
    use super::sanitize_terminal_text;

    #[test]
    fn sanitizes_escape_sequences_for_plain_selection() {
        let text = "alpha\u{1b}[31m red\u{1b}[0m\u{1b}]8;;https://example.com\u{07} link\u{1b}]8;;\u{07}\rbravo";
        assert_eq!(sanitize_terminal_text(text), "alpha red link\nbravo");
    }

    #[test]
    fn sanitizes_backspaces_from_command_output() {
        let text = "buildin\u{08}g ok";
        assert_eq!(sanitize_terminal_text(text), "buildig ok");
    }
}
