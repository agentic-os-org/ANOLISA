// Copyright 2026 Alibaba Cloud
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

//! Retention: did the parts that matter survive compression?
//!
//! This is what separates two similar-looking compression rates. A 99% rate from
//! dropping array items and a 40% rate from re-encoding the same array as a
//! table are not comparable achievements — the first may have discarded the
//! error entry the agent was looking for.
//!
//! Critical items are derived from each payload rather than hand-listed, so they
//! cannot drift from the assets. A scenario that yields none reports zero checks
//! instead of a vacuous 100%: claiming perfect retention because nothing was
//! checked would be worse than admitting the gap.

use serde_json::Value;

use super::asset::{Message, Scenario};

/// How a critical item is looked for after compression.
///
/// Numbers are matched numerically rather than as text: re-rendering an `f64`
/// does not reproduce the digits a JSON writer emitted, so a substring check
/// reports loss for a value that is present and unchanged.
#[derive(Debug, Clone, PartialEq)]
pub enum Check {
    /// Substring that must appear verbatim.
    Text(String),
    /// Value that must still appear, within a relative tolerance.
    Number(f64),
}

impl std::fmt::Display for Check {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Check::Text(t) => write!(f, "{t}"),
            Check::Number(n) => write!(f, "{n}"),
        }
    }
}

/// One fact that must still be findable after compression.
#[derive(Debug, Clone, PartialEq)]
pub struct CriticalItem {
    /// What kind of fact this is, for grouping in the report.
    pub kind: &'static str,
    /// How to look for it.
    pub check: Check,
}

/// Outcome of checking a scenario's critical items.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RetentionScore {
    /// Items still present.
    pub passed: usize,
    /// Items checked.
    pub total: usize,
    /// Needles that went missing, capped for report readability.
    pub missing: Vec<String>,
}

impl RetentionScore {
    /// Share retained, or `None` when nothing was checked.
    ///
    /// `None` rather than 1.0 keeps an unchecked scenario out of the averages
    /// instead of inflating them.
    pub fn rate(&self) -> Option<f64> {
        if self.total == 0 {
            return None;
        }
        Some(self.passed as f64 / self.total as f64)
    }
}

/// How many missing needles to keep, so a fully truncated payload does not
/// produce a report page of near-identical lines.
const MAX_MISSING_REPORTED: usize = 5;

/// Check every critical item against the compressed conversation.
pub fn check(
    items: &[CriticalItem],
    messages: &[Message],
    tools: Option<&[Value]>,
) -> RetentionScore {
    let haystack = haystack(messages, tools);
    // Numbers are scanned once: a payload can hold thousands of them and each
    // item would otherwise rescan the whole text.
    let numbers = if items.iter().any(|i| matches!(i.check, Check::Number(_))) {
        scan_numbers(&haystack)
    } else {
        Vec::new()
    };

    let mut score = RetentionScore {
        total: items.len(),
        ..Default::default()
    };
    for item in items {
        let found = match &item.check {
            Check::Text(needle) => haystack.contains(needle.as_str()),
            Check::Number(value) => numbers.iter().any(|n| close_enough(*n, *value)),
        };
        if found {
            score.passed += 1;
        } else if score.missing.len() < MAX_MISSING_REPORTED {
            score.missing.push(format!("{}: {}", item.kind, item.check));
        }
    }
    score
}

/// Relative tolerance absorbing float re-rendering without matching a
/// genuinely different value.
const NUMERIC_TOLERANCE: f64 = 1e-6;

/// Whether two numbers are the same value under float formatting differences.
fn close_enough(a: f64, b: f64) -> bool {
    if a == b {
        return true;
    }
    let scale = a.abs().max(b.abs()).max(1.0);
    (a - b).abs() / scale < NUMERIC_TOLERANCE
}

/// Every numeric literal in the text, whatever structure surrounds it.
///
/// Deliberately text-level rather than JSON-level: a compressor may hand back a
/// CSV table or a prose summary, and the value still counts as retained.
fn scan_numbers(text: &str) -> Vec<f64> {
    let bytes = text.as_bytes();
    let mut out = Vec::new();
    let mut i = 0usize;
    while i < bytes.len() {
        let c = bytes[i];
        let starts = c.is_ascii_digit()
            || (c == b'-' && i + 1 < bytes.len() && bytes[i + 1].is_ascii_digit());
        if !starts {
            i += 1;
            continue;
        }
        let start = i;
        if bytes[i] == b'-' {
            i += 1;
        }
        while i < bytes.len()
            && (bytes[i].is_ascii_digit()
                || bytes[i] == b'.'
                || bytes[i] == b'e'
                || bytes[i] == b'E'
                || ((bytes[i] == b'+' || bytes[i] == b'-') && matches!(bytes[i - 1], b'e' | b'E')))
        {
            i += 1;
        }
        if let Ok(value) = text[start..i].parse::<f64>() {
            out.push(value);
        }
    }
    out
}

/// Serialize a conversation into one searchable string.
fn haystack(messages: &[Message], tools: Option<&[Value]>) -> String {
    let mut out = String::new();
    for m in messages {
        if let Ok(s) = serde_json::to_string(&Value::Object(m.clone())) {
            out.push_str(&s);
            out.push('\n');
        }
    }
    for t in tools.unwrap_or(&[]) {
        if let Ok(s) = serde_json::to_string(t) {
            out.push_str(&s);
            out.push('\n');
        }
    }
    out
}

/// Derive the critical items of a scenario from its own payload.
///
/// Selection is deliberately conservative: only facts an agent would plausibly
/// need and that can be located unambiguously in the original. Anything fuzzier
/// belongs to the semantic probe, not here.
pub fn critical_items(scenario: &Scenario) -> Vec<CriticalItem> {
    let mut items = Vec::new();

    // The date CacheAligner is meant to stabilise. Losing it changes the
    // model's answer outright, so it is critical wherever it appears.
    for m in &scenario.messages {
        if let Some(text) = m.get("content").and_then(Value::as_str)
            && text.contains("2025-01-06")
        {
            items.push(CriticalItem {
                kind: "date",
                check: Check::Text("2025-01-06".to_string()),
            });
            break;
        }
    }

    // Tool schemas: the name and first required parameter of each tool. A model
    // cannot call a tool whose name was dropped, nor call it correctly without
    // its required parameters.
    for tool in scenario.tools.iter().flatten() {
        let function = tool.get("function").unwrap_or(tool);
        if let Some(name) = function.get("name").and_then(Value::as_str) {
            items.push(CriticalItem {
                kind: "tool_name",
                check: Check::Text(name.to_string()),
            });
        }
        if let Some(required) = function
            .get("parameters")
            .and_then(|p| p.get("required"))
            .and_then(Value::as_array)
            && let Some(first) = required.first().and_then(Value::as_str)
        {
            items.push(CriticalItem {
                kind: "required_param",
                check: Check::Text(first.to_string()),
            });
        }
    }

    // Tool responses: whatever the payload shape makes checkable.
    for m in &scenario.messages {
        if m.get("role").and_then(Value::as_str) != Some("tool") {
            continue;
        }
        let Some(parsed) = m
            .get("content")
            .and_then(Value::as_str)
            .and_then(|c| serde_json::from_str::<Value>(c).ok())
        else {
            continue;
        };
        items.extend(payload_items(&parsed));
    }

    items.dedup();
    items
}

/// Critical items of one parsed tool response.
fn payload_items(value: &Value) -> Vec<CriticalItem> {
    let mut items = Vec::new();
    match value {
        Value::Array(array) => {
            items.extend(error_entries(array));
            items.extend(numeric_extremes(array));
        }
        Value::Object(map) => {
            // Nested arrays: recurse one level, which covers the
            // "object holding several arrays" shape without walking
            // unboundedly deep.
            for nested in map.values() {
                if nested.is_array() {
                    items.extend(payload_items(nested));
                }
            }
        }
        _ => {}
    }
    items
}

/// Entries an agent is most likely reading the output for.
///
/// An error line is the canonical example of a record whose loss changes the
/// outcome: dropping the 900th ordinary log line costs nothing, dropping the one
/// stack trace costs the whole turn.
///
/// Detection reads the fields that *declare* a record to be an error — `level`,
/// `severity`, `status`, and the message itself — rather than scanning the whole
/// serialization. Matching anywhere in the text marks an ordinary search result
/// as critical merely for containing the word "error" in its prose, and such a
/// record then "goes missing" from any truncated array, reporting loss where
/// nothing important was lost.
fn error_entries(array: &[Value]) -> Vec<CriticalItem> {
    let mut items = Vec::new();
    for entry in array {
        if !declares_error(entry) {
            continue;
        }
        // Prefer a distinctive fragment over the whole entry: compression may
        // legitimately reformat surrounding structure while keeping the fact.
        // An entry with no locatable fragment is skipped rather than checked
        // against a needle that would fail on the original too.
        let Some(needle) = distinctive(entry) else {
            continue;
        };
        items.push(CriticalItem {
            kind: "error_entry",
            check: Check::Text(needle),
        });
        if items.len() >= MAX_ERROR_ITEMS {
            break;
        }
    }
    items
}

/// Whether an entry declares itself to be an error.
///
/// A bare string is judged on its own text, since it has no fields to consult;
/// an object is judged only on the fields that carry that meaning.
fn declares_error(entry: &Value) -> bool {
    match entry {
        Value::String(text) => looks_like_error(text),
        Value::Object(map) => ["level", "severity", "status", "message", "msg", "error"]
            .iter()
            .any(|key| {
                map.get(*key)
                    .and_then(Value::as_str)
                    .is_some_and(looks_like_error)
            }),
        _ => false,
    }
}

/// Whether a field value names an error condition.
fn looks_like_error(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    [
        "error", "critical", "fatal", "failed", "failure", "denied", "timeout",
    ]
    .iter()
    .any(|needle| lower.contains(needle))
}

/// Cap on error entries per payload: enough to detect systematic loss without
/// letting one 5000-entry log dominate every average.
const MAX_ERROR_ITEMS: usize = 4;

/// The most identifying fragment of an entry.
///
/// Returns `None` when nothing usable can be extracted. A truncated
/// serialization is explicitly *not* usable: cutting `{"amount":683.59,"crea`
/// out of an object yields a needle that does not occur even in the original
/// payload, so every check against it fails and the score reports loss that
/// never happened.
fn distinctive(entry: &Value) -> Option<String> {
    match entry {
        // A bare string entry is its own identifier, and a prefix of it does
        // occur in the text verbatim.
        Value::String(text) if !text.is_empty() => Some(truncate_needle(text)),
        Value::Object(map) => {
            // A field value that is a whole token in the text, so a prefix of it
            // still matches after reformatting.
            for key in ["message", "msg", "error", "id", "name", "transaction_id"] {
                if let Some(text) = map.get(key).and_then(Value::as_str)
                    && !text.is_empty()
                {
                    return Some(truncate_needle(text));
                }
            }
            // No textual identifier: fall back to the longest string value in
            // the object, which is still a contiguous run of the original text.
            map.values()
                .filter_map(Value::as_str)
                .filter(|s| !s.is_empty())
                .max_by_key(|s| s.len())
                .map(truncate_needle)
        }
        _ => None,
    }
}

/// Longest needle kept. Short enough to survive a compressor reformatting the
/// text around it, long enough to stay unique within one payload.
const MAX: usize = 60;

/// Keep needles short enough to survive reformatting, long enough to be unique.
fn truncate_needle(text: &str) -> String {
    if text.len() <= MAX {
        return text.to_string();
    }
    // Slice on a character boundary so multi-byte text does not panic.
    let end = text
        .char_indices()
        .map(|(i, _)| i)
        .take_while(|i| *i <= MAX)
        .last()
        .unwrap_or(0);
    text[..end].to_string()
}

/// Outlier values a summary must not silently discard.
///
/// Only values far outside the distribution qualify. The extremes of a plain
/// noise series are not facts an agent needs, and asserting on them would mark
/// every array truncation as a loss regardless of whether anything meaningful
/// went missing. A deliberately injected outlier — the kind these fixtures plant
/// to represent an anomaly worth surfacing — sits 7 to 12 sigma out, while the
/// maximum of a few hundred gaussian samples stays near 3, so the threshold
/// separates them cleanly.
///
/// The reference's `number` strategy replaces a series with statistics; whether the
/// injected anomalies survive that is exactly the question worth asking.
fn numeric_extremes(array: &[Value]) -> Vec<CriticalItem> {
    let numbers: Vec<f64> = array.iter().filter_map(Value::as_f64).collect();
    if numbers.len() < MIN_SERIES_LEN {
        return Vec::new();
    }

    let n = numbers.len() as f64;
    let mean = numbers.iter().sum::<f64>() / n;
    let variance = numbers.iter().map(|v| (v - mean).powi(2)).sum::<f64>() / n;
    let std_dev = variance.sqrt();
    if std_dev <= 0.0 || !std_dev.is_finite() {
        return Vec::new();
    }

    let min = numbers.iter().copied().fold(f64::INFINITY, f64::min);
    let max = numbers.iter().copied().fold(f64::NEG_INFINITY, f64::max);

    let mut items = Vec::new();
    for (kind, value) in [("numeric_low_outlier", min), ("numeric_high_outlier", max)] {
        if value.is_finite() && ((value - mean) / std_dev).abs() >= OUTLIER_SIGMA {
            items.push(CriticalItem {
                kind,
                check: Check::Number(value),
            });
        }
    }
    items
}

/// Shortest series worth computing a distribution over.
const MIN_SERIES_LEN: usize = 8;

/// How far out a value must sit to count as a planted anomaly rather than noise.
const OUTLIER_SIGMA: f64 = 5.0;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::l3::Suite;
    use crate::l3::asset::AssetSource;

    fn scenario(messages: Vec<Message>, tools: Option<Vec<Value>>) -> Scenario {
        Scenario {
            suite: Suite::Scenario,
            scenario: "t".into(),
            display_name: None,
            content_type: "json".into(),
            size_label: None,
            source: AssetSource {
                reference: "test".into(),
                headroom_native: true,
                headroom_revision: None,
                headroom_dirty: None,
            },
            headroom_target_ms: None,
            model_limit: 200_000,
            messages,
            tools,
        }
    }

    fn msg(role: &str, content: &str) -> Message {
        let mut m = Message::new();
        m.insert("role".into(), Value::String(role.into()));
        m.insert("content".into(), Value::String(content.into()));
        m
    }

    #[test]
    fn finds_error_entries_in_a_log_array() {
        let logs = serde_json::json!([
            {"level": "INFO", "message": "started"},
            {"level": "ERROR", "message": "connection refused to db-01"},
        ]);
        let s = scenario(vec![msg("tool", &logs.to_string())], None);
        let items = critical_items(&s);
        assert!(
            items.iter().any(|i| i.kind == "error_entry"
                && matches!(&i.check, Check::Text(t) if t.contains("connection refused"))),
            "got {items:?}"
        );
    }

    #[test]
    fn detects_a_dropped_error_entry() {
        // The case that matters: truncation kept the ordinary lines and lost the
        // one that mattered.
        let logs = serde_json::json!([
            {"level": "INFO", "message": "started"},
            {"level": "ERROR", "message": "connection refused to db-01"},
        ]);
        let s = scenario(vec![msg("tool", &logs.to_string())], None);
        let items = critical_items(&s);

        let truncated = serde_json::json!([{"level": "INFO", "message": "started"}]);
        let compressed = vec![msg("tool", &truncated.to_string())];
        let score = check(&items, &compressed, None);
        assert!(score.passed < score.total, "expected a miss: {score:?}");
        assert!(score.missing.iter().any(|m| m.contains("error_entry")));
    }

    #[test]
    fn lossless_reformatting_still_passes() {
        // A table re-encoding changes the surrounding syntax but keeps the fact,
        // so needles must be fragments rather than whole serializations.
        let logs = serde_json::json!([{"level": "ERROR", "message": "disk full on /var"}]);
        let s = scenario(vec![msg("tool", &logs.to_string())], None);
        let items = critical_items(&s);

        let reencoded = vec![msg("tool", "level,message\nERROR,disk full on /var\n")];
        let score = check(&items, &reencoded, None);
        assert_eq!(score.passed, score.total, "missing: {:?}", score.missing);
    }

    #[test]
    fn tracks_the_cache_aligner_date() {
        let s = scenario(
            vec![msg(
                "system",
                "You are helpful.\n\nCurrent date: 2025-01-06",
            )],
            None,
        );
        let items = critical_items(&s);
        assert!(items.iter().any(|i| i.kind == "date"));

        let stripped = vec![msg("system", "You are helpful.")];
        let score = check(&items, &stripped, None);
        assert_eq!(score.passed, 0);
    }

    #[test]
    fn keeps_tool_names_and_required_params() {
        let tool = serde_json::json!({
            "type": "function",
            "function": {
                "name": "search_code",
                "parameters": {"type": "object", "required": ["pattern"]},
            },
        });
        let s = scenario(vec![msg("user", "go")], Some(vec![tool.clone()]));
        let items = critical_items(&s);
        assert!(items.iter().any(|i| i.kind == "tool_name"));
        assert!(items.iter().any(|i| i.kind == "required_param"));

        let score = check(&items, &s.messages, Some(&[tool]));
        assert_eq!(score.passed, score.total);
    }

    #[test]
    fn records_injected_outliers_but_not_noise_extremes() {
        // The fixtures plant anomalies (999.9, -500.0) in an otherwise tight
        // series; those are the facts a summary must not discard. The extremes of
        // a plain noise series are not, and asserting on them would mark every
        // truncation as a loss whether or not anything meaningful went missing.
        let mut planted: Vec<Value> = (0..200)
            .map(|i| Value::from(42.0 + (i % 7) as f64 * 0.5))
            .collect();
        planted[50] = Value::from(999.9);
        planted[150] = Value::from(-500.0);
        let s = scenario(vec![msg("tool", &Value::Array(planted).to_string())], None);
        let items = critical_items(&s);
        assert!(
            items.iter().any(|i| i.kind == "numeric_high_outlier"),
            "planted high outlier not recorded: {items:?}"
        );
        assert!(
            items.iter().any(|i| i.kind == "numeric_low_outlier"),
            "planted low outlier not recorded: {items:?}"
        );

        let noise: Vec<Value> = (0..200)
            .map(|i| Value::from(50.0 + ((i * 37) % 21) as f64 - 10.0))
            .collect();
        let noisy = scenario(vec![msg("tool", &Value::Array(noise).to_string())], None);
        assert!(
            !critical_items(&noisy)
                .iter()
                .any(|i| i.kind.starts_with("numeric")),
            "noise extremes must not be asserted on"
        );
    }

    #[test]
    fn numbers_survive_reformatting() {
        // Re-rendering an f64 does not reproduce a JSON writer's digits, so a
        // substring check would report a present value as lost.
        let items = vec![CriticalItem {
            kind: "numeric_high_outlier",
            check: Check::Number(23.052_741_885_189_69),
        }];
        let reformatted = vec![msg("tool", "value: 23.0527418851896900")];
        let score = check(&items, &reformatted, None);
        assert_eq!(score.passed, 1, "missing: {:?}", score.missing);
    }

    #[test]
    fn a_genuinely_dropped_outlier_is_detected() {
        let items = vec![CriticalItem {
            kind: "numeric_high_outlier",
            check: Check::Number(999.9),
        }];
        let summarised = vec![msg("tool", "count=200 mean=42.1 stddev=2.0")];
        let score = check(&items, &summarised, None);
        assert_eq!(score.passed, 0);
        assert!(score.missing.iter().any(|m| m.contains("outlier")));
    }

    #[test]
    fn number_scan_reads_negatives_and_exponents() {
        let found = scan_numbers("a -500.0 b 1.5e3 c 42 d -2E-2");
        assert!(found.contains(&-500.0), "got {found:?}");
        assert!(found.contains(&1500.0), "got {found:?}");
        assert!(found.contains(&42.0), "got {found:?}");
        assert!(
            found.iter().any(|v| (v + 0.02).abs() < 1e-12),
            "got {found:?}"
        );
    }

    #[test]
    fn prose_containing_the_word_error_is_not_an_error_entry() {
        // A search result whose snippet mentions "error handling" is not an
        // error record. Marking it critical makes it "go missing" from any
        // truncated array, reporting loss where nothing important was lost — the
        // false positive that made nested_object_600 look like a double failure.
        let results = serde_json::json!([
            {"id": "doc_1", "score": 0.7, "title": "Introduction to Async Programming",
             "snippet": "Performance optimization and error handling are discussed."},
        ]);
        let s = scenario(vec![msg("tool", &results.to_string())], None);
        let items = critical_items(&s);
        assert!(
            !items.iter().any(|i| i.kind == "error_entry"),
            "prose mention must not become a critical item: {items:?}"
        );
    }

    #[test]
    fn a_declared_error_level_is_still_detected() {
        let logs = serde_json::json!([
            {"level": "ERROR", "message": "disk full on /var"},
        ]);
        let s = scenario(vec![msg("tool", &logs.to_string())], None);
        assert!(
            critical_items(&s).iter().any(|i| i.kind == "error_entry"),
            "a declared error must still be critical"
        );
    }

    #[test]
    fn an_entry_with_no_locatable_fragment_is_skipped() {
        // These records declare failure via `status` but carry no message-like
        // field. Falling back to a truncated serialization produced a needle
        // absent from the original payload, so the check failed against the
        // uncompressed input and reported loss that never happened.
        let txns = serde_json::json!([
            {"amount": 683.59, "created_at": "2025-01-08T18:16:00Z", "status": "failed"},
        ]);
        let s = scenario(vec![msg("tool", &txns.to_string())], None);
        let items = critical_items(&s);
        let score = check(&items, &s.messages, None);
        assert_eq!(
            score.passed, score.total,
            "the original must retain its own items: {:?}",
            score.missing
        );
    }

    #[test]
    fn falls_back_to_the_longest_string_field() {
        let entries = serde_json::json!([
            {"status": "failed", "detail": "upstream refused the connection"},
        ]);
        let s = scenario(vec![msg("tool", &entries.to_string())], None);
        let items = critical_items(&s);
        assert!(
            items
                .iter()
                .any(|i| matches!(&i.check, Check::Text(t) if t.contains("upstream refused"))),
            "got {items:?}"
        );
    }

    #[test]
    fn unchecked_scenario_reports_none_not_perfect() {
        let s = scenario(vec![msg("user", "hello")], None);
        let items = critical_items(&s);
        let score = check(&items, &s.messages, None);
        assert_eq!(score.total, 0);
        assert_eq!(
            score.rate(),
            None,
            "an unchecked scenario must not score 1.0"
        );
    }

    #[test]
    fn needle_truncation_respects_char_boundaries() {
        let text = "\u{4f60}\u{597d}".repeat(40);
        let needle = truncate_needle(&text);
        assert!(text.starts_with(&needle));
    }
}
