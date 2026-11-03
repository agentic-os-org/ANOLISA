use std::sync::OnceLock;

use regex::Regex;
use serde_json::Value;

use super::{is_sensitive_key, redact_text, sensitive_assignment_pattern};

pub(super) fn redact_private_key_blocks(text: &str) -> (String, bool) {
    let mut ranges = private_key_ranges(text);
    if ranges.is_empty() {
        return (text.to_string(), false);
    }
    if matches!(
        serde_json::from_str::<Value>(text),
        Ok(Value::Object(_) | Value::Array(_))
    ) {
        let Some(json_ranges) = sensitive_json_value_ranges(text, &ranges) else {
            return ("<redacted>".to_string(), true);
        };
        ranges = json_ranges;
    }
    ranges.sort_by_key(|range| range.0);
    let mut merged: Vec<(usize, usize, String)> = Vec::new();
    for (start, end, replacement) in ranges {
        if let Some(previous) = merged.last_mut().filter(|previous| start < previous.1) {
            previous.1 = previous.1.max(end);
            if replacement == "<redacted private key block>" {
                previous.2 = replacement;
            }
        } else {
            merged.push((start, end, replacement));
        }
    }
    let mut output = String::with_capacity(text.len());
    let mut offset = 0;
    for (start, end, replacement) in merged {
        output.push_str(&text[offset..start]);
        output.push_str(&replacement);
        offset = end;
    }
    output.push_str(&text[offset..]);
    (output, true)
}

fn private_key_ranges(text: &str) -> Vec<(usize, usize, String)> {
    let mut ranges = Vec::new();
    let mut pending = None;
    for marker in private_key_markers().captures_iter(text) {
        let matched = marker.get(0).unwrap();
        if marker["kind"].eq_ignore_ascii_case("BEGIN") {
            pending.get_or_insert(matched.start());
        } else if let Some(start) = pending.take() {
            ranges.push((
                start,
                matched.end(),
                "<redacted private key block>".to_string(),
            ));
        }
    }
    if let Some(start) = pending {
        ranges.push((
            start,
            text.len(),
            "<redacted private key block>".to_string(),
        ));
    }
    ranges
}

fn private_key_markers() -> &'static Regex {
    static MARKERS: OnceLock<Regex> = OnceLock::new();
    MARKERS.get_or_init(|| {
        Regex::new(r"-----(?P<kind>(?i-u:BEGIN|END)) [^\r\n]*?(?i-u:PRIVATE KEY)-----").unwrap()
    })
}

fn sensitive_json_value_ranges(
    text: &str,
    private_ranges: &[(usize, usize, String)],
) -> Option<Vec<(usize, usize, String)>> {
    static JSON_STRING: OnceLock<Regex> = OnceLock::new();
    let pattern = JSON_STRING.get_or_init(|| Regex::new(r#""(?:\\.|[^"\\])*""#).unwrap());
    let tokens: Vec<_> = pattern.find_iter(text).collect();
    let mut reserved = std::collections::HashSet::new();
    for token in &tokens {
        if text[token.end()..]
            .trim_start_matches([' ', '\t', '\r', '\n'])
            .starts_with(':')
        {
            reserved.insert(serde_json::from_str::<String>(token.as_str()).ok()?);
        }
    }
    let mut ranges = Vec::new();
    let mut token_index = 0;
    for (start, end, replacement) in private_ranges {
        let mut cursor = *start;
        while token_index < tokens.len() {
            let token = &tokens[token_index];
            #[cfg(test)]
            super::record_work(1, 0, 0);
            if token.end() <= cursor {
                token_index += 1;
                continue;
            }
            if token.start() >= *end {
                break;
            }
            if cursor < token.start() {
                ranges.push((cursor, token.start(), replacement.clone()));
            }
            cursor = token.end().min(*end);
            if token.end() > *end {
                break;
            }
            token_index += 1;
        }
        if cursor < *end {
            ranges.push((cursor, *end, replacement.clone()));
        }
    }
    let mut private_index = 0;
    let mut covered_end = 0;
    let mut next_suffix = std::collections::HashMap::new();
    for token in tokens {
        if token.end() <= covered_end {
            continue;
        }
        let decoded: String = serde_json::from_str(token.as_str()).ok()?;
        let trimmed = text[token.end()..].trim_start_matches([' ', '\t', '\r', '\n']);
        if let Some(value) = trimmed.strip_prefix(':') {
            if is_sensitive_key(&decoded)
                || sensitive_assignment_pattern().is_match(&format!("{decoded}=x"))
            {
                let value = value.trim_start_matches([' ', '\t', '\r', '\n']);
                let start = text.len() - value.len();
                let mut stream =
                    serde_json::Deserializer::from_str(value).into_iter::<serde::de::IgnoredAny>();
                stream.next()?.ok()?;
                covered_end = start + stream.byte_offset();
                #[cfg(test)]
                super::record_work(0, 0, stream.byte_offset());
                // Keep a word separator if a cross-field PEM span leaves non-JSON text.
                ranges.push((
                    start,
                    start + stream.byte_offset(),
                    "\"<redacted>\" ".to_string(),
                ));
            }
        }
        let mut masked = String::from("\"");
        let mut cursor = token.start() + 1;
        let token_end = token.end() - 1;
        while private_index < private_ranges.len() {
            let (start, end, replacement) = &private_ranges[private_index];
            #[cfg(test)]
            super::record_work(1, 0, 0);
            if *end <= cursor {
                private_index += 1;
                continue;
            }
            if *start >= token_end {
                break;
            }
            let start = (*start).max(cursor);
            let end = (*end).min(token_end);
            masked.push_str(&text[cursor..start]);
            masked.push_str(replacement);
            cursor = end;
            if private_ranges[private_index].1 > token_end {
                break;
            }
            private_index += 1;
        }
        masked.push_str(&text[cursor..token_end]);
        masked.push('"');
        let masked: String = serde_json::from_str(&masked).ok()?;
        let mut safe = redact_text(&masked);
        if safe == decoded {
            continue;
        }
        if trimmed.starts_with(':') {
            let base = safe.clone();
            let suffix = next_suffix.entry(base.clone()).or_insert(0usize);
            while reserved.contains(&safe) {
                #[cfg(test)]
                super::record_work(0, 1, 0);
                *suffix += 1;
                safe = format!("{base}-{suffix}");
            }
            reserved.insert(safe.clone());
        }
        ranges.push((
            token.start(),
            token.end(),
            format!("{} ", serde_json::to_string(&safe).ok()?),
        ));
    }
    Some(ranges)
}

pub(super) fn private_key_marker_range(line: &str, marker: &str) -> Option<(usize, usize)> {
    let kind = marker.trim_start_matches('-').trim();
    private_key_markers()
        .captures_iter(line)
        .find_map(|candidate| {
            if candidate["kind"].eq_ignore_ascii_case(kind) {
                let matched = candidate.get(0)?;
                Some((matched.start(), matched.end()))
            } else {
                None
            }
        })
}
