//! Shared markdown section parsing for the generation pipeline.
//!
//! `auto audit --everything` reuses [`markdown_section_body_bounds`] through its
//! `crate::generation::markdown` path so the audit pipeline does not carry a
//! second copy of the same section scanner.

/// Return the heading-inclusive slice and the body-only slice of the markdown
/// section that starts at `header`, ending before the next `## ` heading.
pub(crate) fn split_markdown_section<'a>(
    markdown: &'a str,
    header: &str,
) -> Option<(&'a str, &'a str)> {
    let (body_start, section_end) = markdown_section_body_bounds(markdown, header)?;
    let header_start = body_start - header.len();
    Some((
        &markdown[header_start..section_end],
        &markdown[body_start..section_end],
    ))
}

/// Return the byte offsets `(body_start, section_end)` for the markdown section
/// that starts at `header`, ending before the next `## ` heading.
pub(crate) fn markdown_section_body_bounds(markdown: &str, header: &str) -> Option<(usize, usize)> {
    let start = markdown.find(header)?;
    let body_start = start + header.len();
    let after_header = &markdown[body_start..];
    let section_end = after_header
        .find("\n## ")
        .map(|offset| body_start + offset)
        .unwrap_or(markdown.len());
    Some((body_start, section_end))
}

pub(crate) fn markdown_section_has_nonempty_body(markdown: &str, heading: &str) -> bool {
    markdown_section_contains(markdown, heading, |line| !line.trim().is_empty())
}

pub(crate) fn markdown_section_contains(
    markdown: &str,
    heading: &str,
    predicate: impl Fn(&str) -> bool,
) -> bool {
    let mut in_section = false;
    for line in markdown.lines() {
        let trimmed_end = line.trim_end();
        if trimmed_end == heading {
            in_section = true;
            continue;
        }
        if in_section && trimmed_end.starts_with("## ") {
            return false;
        }
        if in_section && predicate(line) {
            return true;
        }
    }
    false
}

/// Strip a leading `1.` / `1)` ordered-list marker, returning the item body.
pub(crate) fn strip_ordered_list_marker(line: &str) -> Option<&str> {
    let bytes = line.as_bytes();
    let mut index = 0usize;
    while index < bytes.len() && bytes[index].is_ascii_digit() {
        index += 1;
    }
    if index == 0 || index >= bytes.len() {
        return None;
    }
    if bytes[index] != b'.' && bytes[index] != b')' {
        return None;
    }
    index += 1;
    if index >= bytes.len() || !bytes[index].is_ascii_whitespace() {
        return None;
    }
    while index < bytes.len() && bytes[index].is_ascii_whitespace() {
        index += 1;
    }
    Some(&line[index..])
}
