//! Shared LLM JSON-repair primitives.
//!
//! `auto bug` and `auto nemesis` both receive model-authored JSON artifacts
//! that frequently contain unescaped quotes, invalid escapes, fenced blocks, or
//! trailing backend wrapper text. This module owns the byte-identical repair
//! engine both pipelines used to carry in duplicated form. Module-specific
//! shape normalization stays with each caller; only the context-free repair
//! primitives live here.

pub(crate) const JSON_REPAIR_MAX_BYTES: usize = 256 * 1024;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum JsonRepairContext {
    Object(ObjectParseState),
    Array(ArrayParseState),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ObjectParseState {
    KeyOrEnd,
    Colon,
    Value,
    CommaOrEnd,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ArrayParseState {
    ValueOrEnd,
    CommaOrEnd,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum JsonStringRole {
    Key,
    Value,
}

/// Extract the contents of a leading ```` ``` ````-fenced block, if the input
/// is a single fenced block. Returns `None` when there is no fence.
pub(crate) fn extract_fenced_json_block(content: &str) -> Option<String> {
    let trimmed = content.trim();
    if !trimmed.starts_with("```") {
        return None;
    }

    let mut lines = trimmed.lines();
    let opening = lines.next()?.trim();
    if !opening.starts_with("```") {
        return None;
    }

    let mut extracted = String::new();
    let mut saw_closing = false;
    for line in lines {
        if line.trim_start().starts_with("```") {
            saw_closing = true;
            break;
        }
        extracted.push_str(line);
        extracted.push('\n');
    }

    saw_closing.then(|| extracted.trim().to_string())
}

/// Escape unescaped double quotes and invalid backslash escapes inside JSON
/// string tokens. Walks the input as a small JSON state machine so it can tell
/// a string-terminating quote from a quote embedded in prose.
pub(crate) fn escape_unescaped_quotes_in_json_strings(content: &str) -> String {
    let chars = content.chars().collect::<Vec<_>>();
    let mut repaired = String::with_capacity(content.len() + 32);
    let mut contexts = Vec::<JsonRepairContext>::new();
    let mut string_role = None::<JsonStringRole>;
    let mut primitive_value = false;
    let mut index = 0usize;

    while index < chars.len() {
        let ch = chars[index];

        if primitive_value {
            if matches!(ch, ',' | '}' | ']') {
                finish_json_value(&mut contexts);
                primitive_value = false;
                continue;
            }
            repaired.push(ch);
            index += 1;
            continue;
        }

        if let Some(role) = string_role {
            match ch {
                '\\' => {
                    if valid_json_string_escape_at(&chars, index) {
                        repaired.push('\\');
                        repaired.push(chars[index + 1]);
                        if chars[index + 1] == 'u' {
                            repaired.extend(chars[index + 2..index + 6].iter().copied());
                            index += 6;
                        } else {
                            index += 2;
                        }
                    } else {
                        repaired.push('\\');
                        repaired.push('\\');
                        index += 1;
                    }
                    continue;
                }
                '"' => {
                    if is_likely_string_terminator(&chars, index, role, &contexts) {
                        repaired.push(ch);
                        string_role = None;
                        finish_string_token(&mut contexts, role);
                    } else {
                        repaired.push('\\');
                        repaired.push('"');
                    }
                }
                _ => repaired.push(ch),
            }
            index += 1;
            continue;
        }

        match ch {
            '"' => {
                string_role = Some(current_string_role(&contexts));
                repaired.push(ch);
            }
            '{' => {
                contexts.push(JsonRepairContext::Object(ObjectParseState::KeyOrEnd));
                repaired.push(ch);
            }
            '[' => {
                contexts.push(JsonRepairContext::Array(ArrayParseState::ValueOrEnd));
                repaired.push(ch);
            }
            '}' => {
                repaired.push(ch);
                if matches!(contexts.last(), Some(JsonRepairContext::Object(_))) {
                    contexts.pop();
                    finish_json_value(&mut contexts);
                }
            }
            ']' => {
                repaired.push(ch);
                if matches!(contexts.last(), Some(JsonRepairContext::Array(_))) {
                    contexts.pop();
                    finish_json_value(&mut contexts);
                }
            }
            ':' => {
                repaired.push(ch);
                if let Some(JsonRepairContext::Object(state)) = contexts.last_mut() {
                    if *state == ObjectParseState::Colon {
                        *state = ObjectParseState::Value;
                    }
                }
            }
            ',' => {
                repaired.push(ch);
                advance_json_context_after_comma(&mut contexts);
            }
            ch if ch.is_whitespace() => repaired.push(ch),
            _ => {
                repaired.push(ch);
                primitive_value = context_expects_value(&contexts);
            }
        }

        index += 1;
    }

    if primitive_value {
        finish_json_value(&mut contexts);
    }

    repaired
}

fn current_string_role(contexts: &[JsonRepairContext]) -> JsonStringRole {
    match contexts.last() {
        Some(JsonRepairContext::Object(ObjectParseState::KeyOrEnd)) => JsonStringRole::Key,
        _ => JsonStringRole::Value,
    }
}

fn finish_string_token(contexts: &mut [JsonRepairContext], role: JsonStringRole) {
    match role {
        JsonStringRole::Key => {
            if let Some(JsonRepairContext::Object(state)) = contexts.last_mut() {
                *state = ObjectParseState::Colon;
            }
        }
        JsonStringRole::Value => finish_json_value(contexts),
    }
}

fn finish_json_value(contexts: &mut [JsonRepairContext]) {
    if let Some(context) = contexts.last_mut() {
        match context {
            JsonRepairContext::Object(state) if *state == ObjectParseState::Value => {
                *state = ObjectParseState::CommaOrEnd;
            }
            JsonRepairContext::Array(state) if *state == ArrayParseState::ValueOrEnd => {
                *state = ArrayParseState::CommaOrEnd;
            }
            _ => {}
        }
    }
}

fn advance_json_context_after_comma(contexts: &mut [JsonRepairContext]) {
    if let Some(context) = contexts.last_mut() {
        match context {
            JsonRepairContext::Object(state) if *state == ObjectParseState::CommaOrEnd => {
                *state = ObjectParseState::KeyOrEnd;
            }
            JsonRepairContext::Array(state) if *state == ArrayParseState::CommaOrEnd => {
                *state = ArrayParseState::ValueOrEnd;
            }
            _ => {}
        }
    }
}

fn context_expects_value(contexts: &[JsonRepairContext]) -> bool {
    matches!(
        contexts.last(),
        Some(JsonRepairContext::Object(ObjectParseState::Value))
            | Some(JsonRepairContext::Array(ArrayParseState::ValueOrEnd))
            | None
    )
}

fn is_likely_string_terminator(
    chars: &[char],
    quote_index: usize,
    role: JsonStringRole,
    contexts: &[JsonRepairContext],
) -> bool {
    let Some((delimiter_index, delimiter)) = next_significant_char(chars, quote_index + 1) else {
        return role == JsonStringRole::Value;
    };

    match role {
        JsonStringRole::Key => delimiter == ':',
        JsonStringRole::Value => match delimiter {
            '}' | ']' => true,
            ',' => {
                let Some((_, next_token)) = next_significant_char(chars, delimiter_index + 1)
                else {
                    return false;
                };
                match contexts.last() {
                    Some(JsonRepairContext::Object(ObjectParseState::Value)) => next_token == '"',
                    Some(JsonRepairContext::Array(ArrayParseState::ValueOrEnd)) => {
                        is_valid_array_value_start(next_token)
                    }
                    None => false,
                    _ => false,
                }
            }
            _ => false,
        },
    }
}

fn next_significant_char(chars: &[char], mut index: usize) -> Option<(usize, char)> {
    while index < chars.len() {
        let ch = chars[index];
        if !ch.is_whitespace() {
            return Some((index, ch));
        }
        index += 1;
    }
    None
}

fn is_valid_array_value_start(ch: char) -> bool {
    matches!(ch, '"' | '{' | '[' | '-' | 't' | 'f' | 'n') || ch.is_ascii_digit()
}

fn valid_json_string_escape_at(chars: &[char], index: usize) -> bool {
    let Some(next) = chars.get(index + 1).copied() else {
        return false;
    };

    match next {
        '"' | '\\' | '/' | 'b' | 'f' | 'n' | 'r' | 't' => true,
        'u' => chars.get(index + 2..index + 6).is_some_and(|digits| {
            digits.len() == 4 && digits.iter().all(|digit| digit.is_ascii_hexdigit())
        }),
        _ => false,
    }
}

/// When `content` parses as a complete JSON value followed by trailing
/// non-whitespace text (a backend wrapper, log noise), return just the prefix
/// that holds the complete value. Returns `None` when the value spans the whole
/// input.
pub(crate) fn extract_complete_json_value_prefix(content: &str) -> Option<String> {
    let content = content.trim_start();
    let mut stream = serde_json::Deserializer::from_str(content).into_iter::<serde_json::Value>();
    stream.next()?.ok()?;
    let end = stream.byte_offset();
    if content[end..].trim().is_empty() {
        return None;
    }
    Some(content[..end].trim_end().to_string())
}

#[cfg(test)]
mod tests {
    use serde::Deserialize;

    use super::escape_unescaped_quotes_in_json_strings;

    #[derive(Debug, Deserialize)]
    struct SkepticVerdict {
        #[allow(dead_code)]
        bug_id: String,
        counter_argument: String,
    }

    #[test]
    fn repairs_unescaped_quotes_inside_json_strings() {
        let invalid = r#"[
  {
    "bug_id": "BUG-003-02",
    "decision": "disproved",
    "confidence_percent": 95,
    "counter_argument": "The telemetry scraper matches '"message":"bitino-house live funding*'' lines and keeps the txid.",
    "risk_calculation": "Very low risk.",
    "follow_up_checks": ["Check the live logs"]
  }
]"#;

        assert!(serde_json::from_str::<Vec<SkepticVerdict>>(invalid).is_err());

        let repaired = escape_unescaped_quotes_in_json_strings(invalid);
        let parsed = serde_json::from_str::<Vec<SkepticVerdict>>(&repaired)
            .expect("repaired JSON should parse");
        assert_eq!(parsed.len(), 1);
        assert!(parsed[0]
            .counter_argument
            .contains("\"message\":\"bitino-house live funding*"));
    }

    #[test]
    fn repairs_invalid_backslash_escapes_inside_json_strings() {
        let invalid = r#"[
  {
    "bug_id": "BUG-003-03",
    "decision": "disproved",
    "confidence_percent": 95,
    "counter_argument": "The matcher still treats \d+\_suffix as a literal pattern fragment.",
    "risk_calculation": "Very low risk.",
    "follow_up_checks": ["Check the live logs"]
  }
]"#;

        assert!(serde_json::from_str::<Vec<SkepticVerdict>>(invalid).is_err());

        let repaired = escape_unescaped_quotes_in_json_strings(invalid);
        let parsed = serde_json::from_str::<Vec<SkepticVerdict>>(&repaired)
            .expect("repaired JSON should parse");
        assert_eq!(parsed.len(), 1);
        assert!(parsed[0].counter_argument.contains("\\d+\\_suffix"));
    }

    #[test]
    fn repairs_invalid_unicode_escapes_inside_json_strings() {
        let invalid = r#"[
  {
    "bug_id": "BUG-003-04",
    "decision": "disproved",
    "confidence_percent": 95,
    "counter_argument": "The note includes \u12G4 as a literal token from the log output.",
    "risk_calculation": "Very low risk.",
    "follow_up_checks": ["Check the live logs"]
  }
]"#;

        assert!(serde_json::from_str::<Vec<SkepticVerdict>>(invalid).is_err());

        let repaired = escape_unescaped_quotes_in_json_strings(invalid);
        let parsed = serde_json::from_str::<Vec<SkepticVerdict>>(&repaired)
            .expect("repaired JSON should parse");
        assert_eq!(parsed.len(), 1);
        assert!(parsed[0].counter_argument.contains("\\u12G4"));
    }
}
