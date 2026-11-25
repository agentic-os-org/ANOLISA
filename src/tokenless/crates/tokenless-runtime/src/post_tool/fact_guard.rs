//! Error-path invariant checks ensuring compression never drops diagnostic facts.

use std::collections::HashSet;

use serde_json::Value;
use tokenless_compressors::clean_terminal;

/// Memory cap per anchor category; exceeding it triggers fail-safe [`FactViolation::CapacityExceeded`].
const MAX_ANCHORS_PER_KIND: usize = 256;
/// Recursion guard for structured JSON/TOON string-leaf extraction.
const MAX_JSON_WALK_DEPTH: usize = 64;

/// Reason an error-path compression candidate was rolled back by [`verify`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FactViolation {
    /// A structured diagnostic code (such as `error[E0308]`) disappeared.
    DiagnosticCode,
    /// A test or build summary line disappeared.
    SummaryLine,
    /// A failing test identifier disappeared.
    FailingTest,
    /// A `file:line` anchor on a diagnostic line disappeared.
    DiagnosticLocation,
    /// A structured assertion, panic, or exception diagnostic disappeared.
    DiagnosticSignal,
    /// Anchor count exceeded the bounded tracking cap; fail safe to passthrough.
    CapacityExceeded,
}

impl FactViolation {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::DiagnosticCode => "diagnostic_code",
            Self::SummaryLine => "summary_line",
            Self::FailingTest => "failing_test",
            Self::DiagnosticLocation => "diagnostic_location",
            Self::DiagnosticSignal => "diagnostic_signal",
            Self::CapacityExceeded => "capacity_exceeded",
        }
    }
}

#[derive(Default)]
struct FactAnchors {
    diagnostic_codes: HashSet<String>,
    summary_lines: HashSet<String>,
    failing_tests: HashSet<String>,
    file_lines: HashSet<String>,
    diagnostic_signals: HashSet<String>,
    overflowed: bool,
}

/// Verify that every extracted failure anchor in `original` still exists in `candidate`.
///
/// SGR escape codes are stripped from both inputs first so ANSI cleanup never counts
/// as dropping diagnostic text. When `original` parses as a JSON object or array,
/// anchors are extracted from decoded string leaves in both `original` and `candidate`
/// (parsing `candidate` as JSON or TOON), falling back to raw lines if unparsed.
pub(crate) fn verify(original: &str, candidate: &str) -> Result<(), FactViolation> {
    let clean_orig = clean_terminal(original);
    let clean_cand = clean_terminal(candidate);

    let orig_json = parse_json_container(&clean_orig);
    let orig = if let Some(value) = orig_json.as_ref() {
        let mut anchors = FactAnchors::default();
        extract_json_strings(value, 0, &mut anchors);
        anchors
    } else {
        extract(&clean_orig)
    };

    let cand = if orig_json.is_some() {
        let cand_value = parse_json_container(&clean_cand)
            .or_else(|| toon_format::decode_default(&clean_cand).ok());
        if let Some(value) = cand_value {
            let mut anchors = FactAnchors::default();
            extract_json_strings(&value, 0, &mut anchors);
            anchors
        } else {
            // Unparsable candidates fall back to raw line extraction; when structured
            // leaf anchors are missing from raw prose, verification fails safe below.
            extract(&clean_cand)
        }
    } else {
        extract(&clean_cand)
    };

    if orig.overflowed || cand.overflowed {
        return Err(FactViolation::CapacityExceeded);
    }
    if !orig.diagnostic_codes.is_subset(&cand.diagnostic_codes) {
        return Err(FactViolation::DiagnosticCode);
    }
    if !orig.summary_lines.is_subset(&cand.summary_lines) {
        return Err(FactViolation::SummaryLine);
    }
    if !orig.failing_tests.is_subset(&cand.failing_tests) {
        return Err(FactViolation::FailingTest);
    }
    if !orig.file_lines.is_subset(&cand.file_lines) {
        return Err(FactViolation::DiagnosticLocation);
    }
    if !orig.diagnostic_signals.is_subset(&cand.diagnostic_signals) {
        return Err(FactViolation::DiagnosticSignal);
    }
    Ok(())
}

fn parse_json_container(text: &str) -> Option<Value> {
    let trimmed = text.trim_start();
    if !trimmed.starts_with('{') && !trimmed.starts_with('[') {
        return None;
    }
    serde_json::from_str(text).ok()
}

fn extract_json_strings(value: &Value, depth: usize, anchors: &mut FactAnchors) {
    if depth >= MAX_JSON_WALK_DEPTH {
        return;
    }
    match value {
        Value::String(text) => {
            let cleaned = clean_terminal(text);
            if depth + 1 < MAX_JSON_WALK_DEPTH
                && let Some(nested) = parse_json_container(&cleaned)
            {
                extract_json_strings(&nested, depth + 1, anchors);
            } else {
                extract_into(&cleaned, anchors);
            }
        }
        Value::Array(items) => {
            for item in items {
                extract_json_strings(item, depth + 1, anchors);
            }
        }
        Value::Object(map) => {
            for item in map.values() {
                extract_json_strings(item, depth + 1, anchors);
            }
        }
        Value::Null | Value::Bool(_) | Value::Number(_) => {}
    }
}

fn insert_bounded(set: &mut HashSet<String>, overflowed: &mut bool, item: String) {
    if set.contains(&item) {
        return;
    }
    if set.len() < MAX_ANCHORS_PER_KIND {
        set.insert(item);
    } else {
        *overflowed = true;
    }
}

fn extract(text: &str) -> FactAnchors {
    let mut anchors = FactAnchors::default();
    extract_into(text, &mut anchors);
    anchors
}

fn extract_into(text: &str, anchors: &mut FactAnchors) {
    let mut active_search_file: Option<&str> = None;

    for raw_line in text.lines() {
        let trimmed = raw_line.trim();
        if let Some(rest) = trimmed.strip_prefix("File=\"")
            && let Some(path) = rest.strip_suffix('"')
        {
            active_search_file = Some(path);
            continue;
        }
        if trimmed.is_empty() {
            active_search_file = None;
            continue;
        }

        extract_diagnostic_codes(
            trimmed,
            &mut anchors.diagnostic_codes,
            &mut anchors.overflowed,
        );
        if is_summary_line(trimmed) {
            insert_bounded(
                &mut anchors.summary_lines,
                &mut anchors.overflowed,
                trimmed.to_owned(),
            );
        }
        extract_failing_tests(trimmed, &mut anchors.failing_tests, &mut anchors.overflowed);
        extract_file_lines(
            trimmed,
            active_search_file,
            &mut anchors.file_lines,
            &mut anchors.overflowed,
        );
        extract_diagnostic_signals(
            trimmed,
            &mut anchors.diagnostic_signals,
            &mut anchors.overflowed,
        );
    }
}

fn extract_diagnostic_codes(line: &str, out: &mut HashSet<String>, overflowed: &mut bool) {
    let mut rest = line;
    while let Some(pos) = rest.find("error[E") {
        let after = &rest[pos + "error[E".len()..];
        let digits_len = after.bytes().take_while(u8::is_ascii_digit).count();
        if digits_len > 0 && after[digits_len..].starts_with(']') {
            let end = "error[E".len() + digits_len + 1;
            insert_bounded(out, overflowed, rest[pos..pos + end].to_owned());
            rest = &rest[pos + end..];
        } else {
            rest = &rest[pos + 1..];
        }
    }

    let mut ts_rest = line;
    while let Some(pos) = ts_rest.find("error TS") {
        let after = &ts_rest[pos + "error TS".len()..];
        let digits_len = after.bytes().take_while(u8::is_ascii_digit).count();
        if digits_len >= 4 {
            let code = &ts_rest[pos + "error ".len()..pos + "error TS".len() + digits_len];
            insert_bounded(out, overflowed, code.to_owned());
            ts_rest = &ts_rest[pos + "error TS".len() + digits_len..];
        } else {
            ts_rest = &ts_rest[pos + 1..];
        }
    }
}

fn is_summary_line(trimmed: &str) -> bool {
    trimmed.starts_with("test result:")
        || trimmed.starts_with("FAILED (")
        || (trimmed.starts_with("===")
            && (trimmed.contains(" failed") || trimmed.contains(" error")))
        || (trimmed.starts_with("Tests:") && trimmed.contains("failed"))
        || (trimmed.starts_with("Test Suites:") && trimmed.contains("failed"))
        || trimmed.starts_with("FAIL\t")
        || trimmed == "BUILD FAILED"
        || trimmed.starts_with("BUILD FAILURE")
        || trimmed.starts_with("ninja: build stopped:")
        || trimmed.starts_with("make: ***")
}

fn is_ident_boundary_before(text: &str, pos: usize) -> bool {
    pos == 0
        || text[..pos].chars().next_back().is_some_and(|ch| {
            !ch.is_ascii_alphanumeric() && !matches!(ch, '_' | ':' | '-' | '/' | '.')
        })
}

fn clean_embedded_test_token(raw: &str) -> &str {
    raw.split(|ch: char| ch.is_whitespace() || matches!(ch, '"' | '\'' | ',' | '}' | ']'))
        .next()
        .unwrap_or("")
}

fn extract_failing_tests(trimmed: &str, out: &mut HashSet<String>, overflowed: &mut bool) {
    let mut failed_cursor = 0;
    while let Some(rel_end) = trimmed[failed_cursor..].find(" ... FAILED") {
        let failed_pos = failed_cursor + rel_end;
        let prefix = &trimmed[..failed_pos];
        if let Some(rel_start) = prefix.rfind("test ")
            && is_ident_boundary_before(prefix, rel_start)
        {
            let clean = prefix[rel_start + "test ".len()..].trim();
            if !clean.is_empty()
                && !clean
                    .chars()
                    .any(|ch| ch.is_whitespace() || matches!(ch, '"' | '\''))
            {
                insert_bounded(out, overflowed, clean.to_owned());
            }
        }
        failed_cursor = failed_pos + " ... FAILED".len();
    }

    let mut stdout_cursor = 0;
    while let Some(rel_end) = trimmed[stdout_cursor..].find(" stdout ----") {
        let end_pos = stdout_cursor + rel_end;
        let prefix = &trimmed[..end_pos];
        if let Some(rel_start) = prefix.rfind("---- ")
            && is_ident_boundary_before(prefix, rel_start)
        {
            let clean = prefix[rel_start + "---- ".len()..].trim();
            if !clean.is_empty()
                && !clean
                    .chars()
                    .any(|ch| ch.is_whitespace() || matches!(ch, '"' | '\''))
            {
                insert_bounded(out, overflowed, clean.to_owned());
            }
        }
        stdout_cursor = end_pos + " stdout ----".len();
    }

    if let Some(rest) = trimmed.strip_prefix("FAILED ")
        && !rest.starts_with('(')
        && let Some(node) = rest.split_whitespace().next()
        && !node.is_empty()
    {
        insert_bounded(out, overflowed, node.to_owned());
    }

    let mut go_cursor = 0;
    while let Some(rel_start) = trimmed[go_cursor..].find("--- FAIL: ") {
        let start = go_cursor + rel_start;
        if is_ident_boundary_before(trimmed, start) {
            let name = clean_embedded_test_token(&trimmed[start + "--- FAIL: ".len()..]);
            if !name.is_empty() {
                insert_bounded(out, overflowed, name.to_owned());
            }
        }
        go_cursor = start + "--- FAIL: ".len();
    }

    if let Some(rest) = trimmed.strip_prefix("● ") {
        let clean = rest.trim();
        if !clean.is_empty() {
            insert_bounded(out, overflowed, clean.to_owned());
        }
    }
    if let Some(rest) = trimmed.strip_prefix("FAIL ")
        && let Some(path) = rest.split_whitespace().next()
        && !path.is_empty()
    {
        insert_bounded(out, overflowed, path.to_owned());
    }
    if (trimmed.contains("***Failed") || trimmed.contains("***Timeout"))
        && let Some((_, after_hash)) = trimmed.split_once("Test #")
        && let Some((_, after_colon)) = after_hash.split_once(':')
        && let Some(name) = after_colon.split_whitespace().next()
        && !name.is_empty()
    {
        insert_bounded(out, overflowed, name.to_owned());
    }
}

fn is_pascal_exception_head(trimmed: &str) -> bool {
    let candidate = trimmed
        .strip_prefix("E   ")
        .or_else(|| trimmed.strip_prefix(">   "))
        .or_else(|| trimmed.strip_prefix("Caused by: "))
        .unwrap_or(trimmed)
        .trim_start();
    let mut bytes = candidate.bytes();
    let Some(first) = bytes.next() else {
        return false;
    };
    if !first.is_ascii_uppercase() {
        return false;
    }
    let word_len = candidate
        .bytes()
        .take_while(|byte| byte.is_ascii_alphanumeric())
        .count();
    let word = &candidate[..word_len];
    let after = &candidate[word_len..];
    (word.ends_with("Error") || word.ends_with("Exception"))
        && (after.is_empty() || after.starts_with(':'))
}

fn extract_diagnostic_signals(trimmed: &str, out: &mut HashSet<String>, overflowed: &mut bool) {
    if trimmed.contains("AssertionError")
        || trimmed.contains("panicked at ")
        || is_pascal_exception_head(trimmed)
    {
        insert_bounded(out, overflowed, trimmed.to_owned());
    }
}

fn is_diagnostic_context_line(trimmed: &str) -> bool {
    trimmed.starts_with("--> ")
        || trimmed.starts_with("File \"")
        || trimmed.contains("panicked at ")
        || trimmed.starts_with("at ")
        || trimmed.contains("Traceback")
        || trimmed.contains("AssertionError")
        || is_pascal_exception_head(trimmed)
        || [
            "error",
            "Error",
            "ERROR",
            "FAIL",
            "fail",
            "panic",
            "Exception",
            "exception",
        ]
        .iter()
        .any(|needle| trimmed.contains(needle))
}

fn is_valid_path_char(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'/' | b'\\' | b'.' | b'_' | b'-')
}

fn extract_file_lines(
    trimmed: &str,
    active_search_file: Option<&str>,
    out: &mut HashSet<String>,
    overflowed: &mut bool,
) {
    if let Some(file) = active_search_file {
        let digits_len = trimmed.bytes().take_while(u8::is_ascii_digit).count();
        if digits_len > 0 && trimmed[digits_len..].starts_with(':') {
            let body = trimmed[digits_len + 1..].trim();
            if is_diagnostic_context_line(body) {
                insert_bounded(
                    out,
                    overflowed,
                    format!("{file}:{}", &trimmed[..digits_len]),
                );
            }
            return;
        }
    }

    if !is_diagnostic_context_line(trimmed) {
        return;
    }

    if let Some(rest) = trimmed.strip_prefix("File \"")
        && let Some((path, after_path)) = rest.split_once("\", line ")
    {
        let digits_len = after_path.bytes().take_while(u8::is_ascii_digit).count();
        if digits_len > 0 && !path.is_empty() {
            insert_bounded(
                out,
                overflowed,
                format!("{path}:{}", &after_path[..digits_len]),
            );
        }
    }

    let bytes = trimmed.as_bytes();
    for (idx, &byte) in bytes.iter().enumerate() {
        if byte != b':' || idx == 0 {
            continue;
        }
        let line_digits = bytes[idx + 1..]
            .iter()
            .take_while(|b| b.is_ascii_digit())
            .count();
        if line_digits == 0 {
            continue;
        }
        let after_digits = idx + 1 + line_digits;
        if let Some(&next) = bytes.get(after_digits)
            && !matches!(next, b':' | b' ' | b'\t' | b')' | b',' | b']')
        {
            continue;
        }
        let mut start = idx;
        while start > 0 && is_valid_path_char(bytes[start - 1]) {
            start -= 1;
        }
        let candidate_path = &trimmed[start..idx];
        if (candidate_path.contains('.') || candidate_path.contains('/'))
            && !matches!(candidate_path, "http" | "https")
        {
            insert_bounded(
                out,
                overflowed,
                format!(
                    "{candidate_path}:{}",
                    &trimmed[idx + 1..idx + 1 + line_digits]
                ),
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preserves_error_codes_summaries_failing_tests_and_locations() {
        let orig = "\u{1b}[31merror[E0308]\u{1b}[0m: mismatched types\n\
             --> src/main.rs:42:9\n\
             test alpha::broken ... FAILED\n\
             ---- alpha::broken stdout ----\n\
             thread 'alpha::broken' panicked at src/lib.rs:17:5:\n\
             test result: FAILED. 10 passed; 1 failed\n";
        let cand = "error[E0308]: mismatched types\n\
             --> src/main.rs:42:9\n\
             test alpha::broken ... FAILED\n\
             ---- alpha::broken stdout ----\n\
             thread 'alpha::broken' panicked at src/lib.rs:17:5:\n\
             test result: FAILED. 10 passed; 1 failed\n";
        assert_eq!(verify(orig, cand), Ok(()));
    }

    #[test]
    fn rejects_dropped_code_summary_test_or_location() {
        let orig = "error[E0308]: mismatched types\n\
             --> src/main.rs:42:9\n\
             FAILED tests/test_a.py::test_x - AssertionError\n\
             test result: FAILED. 0 passed; 1 failed\n";
        assert_eq!(
            verify(
                orig,
                "--> src/main.rs:42:9\nFAILED tests/test_a.py::test_x\nAssertionError\ntest result: FAILED. 0 passed; 1 failed\n"
            ),
            Err(FactViolation::DiagnosticCode)
        );
        assert_eq!(
            verify(
                orig,
                "error[E0308]\n--> src/main.rs:42:9\nFAILED tests/test_a.py::test_x\nAssertionError\n"
            ),
            Err(FactViolation::SummaryLine)
        );
        assert_eq!(
            verify(
                orig,
                "error[E0308]\n--> src/main.rs:42:9\nAssertionError\ntest result: FAILED. 0 passed; 1 failed\n"
            ),
            Err(FactViolation::FailingTest)
        );
        assert_eq!(
            verify(
                orig,
                "error[E0308]\nFAILED tests/test_a.py::test_x\nAssertionError\ntest result: FAILED. 0 passed; 1 failed\n"
            ),
            Err(FactViolation::DiagnosticLocation)
        );
    }

    #[test]
    fn search_file_header_format_preserves_diagnostic_file_line() {
        let orig = "src/lib.rs:12:error[E0425]: cannot find value\n";
        let cand = "File=\"src/lib.rs\"\n12:error[E0425]: cannot find value\n";
        assert_eq!(verify(orig, cand), Ok(()));
    }

    #[test]
    fn json_record_reduction_dropping_embedded_test_and_assertion_is_rejected() {
        let orig = serde_json::json!([
            {"id": 0, "msg": "ok"},
            {"id": 1, "msg": "test api::rejects_bad_input ... FAILED"},
            {"id": 2, "msg": "AssertionError: expected 400, received 200 for malformed request"}
        ])
        .to_string();
        let dropped_both = serde_json::json!([{"id": 0, "msg": "ok"}]).to_string();
        assert_eq!(
            verify(&orig, &dropped_both),
            Err(FactViolation::FailingTest)
        );

        let dropped_assertion = serde_json::json!([
            {"id": 0, "msg": "ok"},
            {"id": 1, "msg": "test api::rejects_bad_input ... FAILED"}
        ])
        .to_string();
        assert_eq!(
            verify(&orig, &dropped_assertion),
            Err(FactViolation::DiagnosticSignal)
        );
    }

    #[test]
    fn lossless_toon_encoding_of_error_json_passes_fact_guard() {
        let value = serde_json::json!([
            {"id": 0, "msg": "ok"},
            {"id": 1, "msg": "test api::rejects_bad_input ... FAILED"},
            {"id": 2, "msg": "AssertionError: expected 400, received 200 for malformed request"}
        ]);
        let orig = value.to_string();
        let toon = toon_format::encode_default(&value).unwrap();
        assert_eq!(verify(&orig, &toon), Ok(()));
    }

    #[test]
    fn overflowing_anchor_cap_fails_safe_with_capacity_exceeded() {
        let mut lines = String::new();
        for i in 0..300 {
            lines.push_str(&format!("src/file_{i}.rs:{i}: error: failed\n"));
        }
        assert_eq!(verify(&lines, &lines), Err(FactViolation::CapacityExceeded));
    }

    #[test]
    fn json_string_leaf_strips_sgr_before_matching_exception_and_allows_lossless_toon() {
        let value = serde_json::json!([
            {"id": 0, "msg": "ok"},
            {"id": 1, "msg": "\u{1b}[31mRuntimeError: failed to load configuration\u{1b}[0m"}
        ]);
        let orig = value.to_string();
        let dropped = serde_json::json!([{"id": 0, "msg": "ok"}]).to_string();
        assert_eq!(
            verify(&orig, &dropped),
            Err(FactViolation::DiagnosticSignal)
        );

        let toon = toon_format::encode_default(&value).unwrap();
        assert_eq!(verify(&orig, &toon), Ok(()));
    }

    #[test]
    fn nested_json_string_leaf_extracts_exception_and_allows_lossless_toon() {
        let nested_payload =
            serde_json::json!({"msg": "\u{1b}[31mRuntimeError: failed to load configuration\u{1b}[0m"})
                .to_string();
        let value = serde_json::json!([
            {"id": 0, "msg": "ok"},
            {"id": 1, "msg": nested_payload}
        ]);
        let orig = value.to_string();
        let dropped = serde_json::json!([{"id": 0, "msg": "ok"}]).to_string();
        assert_eq!(
            verify(&orig, &dropped),
            Err(FactViolation::DiagnosticSignal)
        );

        let toon = toon_format::encode_default(&value).unwrap();
        assert_eq!(verify(&orig, &toon), Ok(()));
    }

    #[test]
    fn json_original_with_unparsable_truncated_candidate_fails_safe() {
        let orig = serde_json::json!([
            {"id": 0, "msg": "RuntimeError: failed to load configuration"}
        ])
        .to_string();
        let unparsable = "[{\"id\":0,\"msg\":\"RuntimeError: failed to load configuration\"";
        assert_eq!(
            verify(&orig, unparsable),
            Err(FactViolation::DiagnosticSignal)
        );
    }
}
