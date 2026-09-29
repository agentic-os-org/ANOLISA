//! Signal detection: turns unified event rows into [`PreferenceSignal`]s.
//!
//! Pure keyword/statistical rules over a query window — no persistence and
//! no LLM calls. Rows arrive in the source-agnostic [`PreferenceEventRow`]
//! shape produced by the data-source providers (`genai_source`,
//! `trajectory_source`), so nothing here knows where a turn came from. All
//! thresholds and keyword tables live in [`super::signals`] so rule tuning
//! never touches this file.

use std::collections::BTreeMap;

use super::signals::{
    CJK_RATIO_THRESHOLD, CORRECTION_PATTERNS, CORRECTION_WINDOW_NS, MIN_QUERY_CHARS_FOR_LANGUAGE,
    PLAN_FIRST_PATTERNS, PreferenceSignal, SignalKind, TEST_REQUIREMENT_PATTERNS, truncate_excerpt,
};

/// One analysis turn in the unified, source-agnostic shape both data-source
/// providers produce (`genai_source` for `genai_events.db`,
/// `trajectory_source` for `trajectories.db`).
#[derive(Debug, Clone)]
pub struct PreferenceEventRow {
    /// Source row id (genai) or synthetic sequence number (trajectory) —
    /// only used as signal provenance.
    pub id: i64,
    pub session_id: Option<String>,
    /// Grouping key for cross-turn rules: the genai `conversation_id` or
    /// the trajectory session id.
    pub conversation_id: Option<String>,
    /// Per-turn timestamp in epoch nanoseconds. `None` when the source has
    /// no reliable value (e.g. an ATIF step without a parseable timestamp);
    /// time-gap rules skip such turns instead of running on made-up values.
    pub timestamp_ns: Option<i64>,
    /// User message text with source-specific template noise already
    /// stripped by the provider. `None` when nothing usable is left.
    pub user_text: Option<String>,
    /// Names of the tool calls the turn's response issued.
    pub tool_names: Vec<String>,
}

/// Extract all preference signals from a batch of event rows.
///
/// Per-row rules (language, plan-first, testing, correction keywords, tool
/// calls) are applied independently; the rapid follow-up correction rule
/// needs cross-row context and runs over the whole batch. Malformed message
/// JSON is tolerated: the row is skipped with a debug log, never an error.
pub fn detect_signals(rows: &[PreferenceEventRow]) -> Vec<PreferenceSignal> {
    let mut out = Vec::new();
    for row in rows {
        detect_row_signals(row, &mut out);
    }
    detect_rapid_followups(rows, &mut out);
    out
}

/// Rules that only need a single row.
fn detect_row_signals(row: &PreferenceEventRow, out: &mut Vec<PreferenceSignal>) {
    if let Some(query) = row.user_text.as_deref() {
        let excerpt = truncate_excerpt(query);
        if let Some(language) = detect_language(query) {
            out.push(make_signal(
                row,
                SignalKind::Language { language },
                &excerpt,
            ));
        }
        let lowered = query.to_lowercase();
        if matches_any(&lowered, PLAN_FIRST_PATTERNS) {
            out.push(make_signal(row, SignalKind::PlanFirst, &excerpt));
        }
        if matches_any(&lowered, TEST_REQUIREMENT_PATTERNS) {
            out.push(make_signal(row, SignalKind::TestRequirement, &excerpt));
        }
        if matches_any(&lowered, CORRECTION_PATTERNS) {
            out.push(make_signal(row, SignalKind::Correction, &excerpt));
        }
    }
    detect_tool_calls(row, out);
}

fn make_signal(row: &PreferenceEventRow, kind: SignalKind, excerpt: &str) -> PreferenceSignal {
    PreferenceSignal {
        kind,
        event_id: row.id,
        session_id: row.session_id.clone(),
        timestamp_ns: row.timestamp_ns,
        excerpt: excerpt.to_string(),
    }
}

/// Classify the dominant natural language of a query.
///
/// Counts CJK ideographs versus ASCII letters; punctuation, digits, and
/// whitespace are ignored so code snippets bias the ratio less. Returns
/// `None` for texts too short to classify.
fn detect_language(text: &str) -> Option<&'static str> {
    let mut cjk = 0usize;
    let mut ascii_alpha = 0usize;
    for c in text.chars() {
        if is_cjk(c) {
            cjk += 1;
        } else if c.is_ascii_alphabetic() {
            ascii_alpha += 1;
        }
    }
    let total = cjk + ascii_alpha;
    if total < MIN_QUERY_CHARS_FOR_LANGUAGE {
        return None;
    }
    if (cjk as f64) / (total as f64) >= CJK_RATIO_THRESHOLD {
        Some("chinese")
    } else {
        Some("english")
    }
}

/// True for CJK Unified Ideographs (base block + Extension A) — enough to
/// separate Chinese from English queries without a full Unicode table.
fn is_cjk(c: char) -> bool {
    matches!(c, '\u{4E00}'..='\u{9FFF}' | '\u{3400}'..='\u{4DBF}')
}

fn matches_any(lowered_text: &str, patterns: &[&str]) -> bool {
    patterns.iter().any(|p| lowered_text.contains(p))
}

/// One `ToolUsage` signal per tool call the providers extracted into
/// `tool_names`, so call frequency directly becomes evidence count during
/// aggregation.
fn detect_tool_calls(row: &PreferenceEventRow, out: &mut Vec<PreferenceSignal>) {
    for name in &row.tool_names {
        if !name.is_empty() {
            let excerpt = truncate_excerpt(&format!("tool_call: {name}"));
            out.push(make_signal(
                row,
                SignalKind::ToolUsage {
                    tool_name: name.clone(),
                },
                &excerpt,
            ));
        }
    }
}

/// Cross-row correction rule: within one conversation, a *changed* user
/// query arriving shortly after the previous turn means the user cut the
/// assistant off to redirect it. Adjacent events with identical queries are
/// the agent's own follow-up calls for the same turn, not corrections.
/// Rows without a reliable timestamp cannot prove the time gap and are
/// excluded rather than compared on fabricated values.
fn detect_rapid_followups(rows: &[PreferenceEventRow], out: &mut Vec<PreferenceSignal>) {
    // Group by conversation_id, keeping only rows with a usable query and
    // a real timestamp.
    let mut by_conversation: BTreeMap<&str, Vec<(&PreferenceEventRow, &str, i64)>> =
        BTreeMap::new();
    for row in rows {
        let Some(conversation_id) = row.conversation_id.as_deref() else {
            continue;
        };
        let Some(query) = row.user_text.as_deref() else {
            continue;
        };
        let Some(ts) = row.timestamp_ns else {
            continue;
        };
        by_conversation
            .entry(conversation_id)
            .or_default()
            .push((row, query, ts));
    }

    for events in by_conversation.values_mut() {
        events.sort_by_key(|(_, _, ts)| *ts);
        for pair in events.windows(2) {
            let (_, prev_query, prev_ts) = &pair[0];
            let (curr, curr_query, curr_ts) = &pair[1];
            if curr_query == prev_query {
                continue;
            }
            let gap = curr_ts.saturating_sub(*prev_ts);
            if gap > 0 && gap < CORRECTION_WINDOW_NS {
                out.push(make_signal(
                    curr,
                    SignalKind::Correction,
                    &truncate_excerpt(curr_query),
                ));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(id: i64, query: &str) -> PreferenceEventRow {
        PreferenceEventRow {
            id,
            session_id: Some("s1".to_string()),
            conversation_id: Some("c1".to_string()),
            timestamp_ns: Some(id * 1_000_000_000),
            user_text: Some(query.to_string()),
            tool_names: Vec::new(),
        }
    }

    fn kinds_of(rows: &[PreferenceEventRow]) -> Vec<SignalKind> {
        detect_signals(rows).into_iter().map(|s| s.kind).collect()
    }

    #[test]
    fn empty_input_yields_no_signals() {
        assert!(detect_signals(&[]).is_empty());
        let empty = PreferenceEventRow {
            id: 1,
            session_id: None,
            conversation_id: None,
            timestamp_ns: Some(0),
            user_text: None,
            tool_names: Vec::new(),
        };
        assert!(detect_signals(&[empty]).is_empty());
    }

    #[test]
    fn detects_chinese_and_english_language() {
        assert_eq!(
            detect_language("请帮我重构这个模块的错误处理"),
            Some("chinese")
        );
        assert_eq!(
            detect_language("please refactor the error handling"),
            Some("english")
        );
        // Mixed developer query: enough CJK to count as Chinese.
        assert_eq!(
            detect_language("帮我修一下 parse_event 的 bug"),
            Some("chinese")
        );
        // Too short to classify.
        assert_eq!(detect_language("ok"), None);
        assert_eq!(detect_language(""), None);
    }

    #[test]
    fn detects_workflow_and_test_patterns_bilingual() {
        let kinds = kinds_of(&[row(1, "先计划再动手，别急着写代码")]);
        assert!(kinds.contains(&SignalKind::PlanFirst));

        let kinds = kinds_of(&[row(2, "Please make a plan first before writing code")]);
        assert!(kinds.contains(&SignalKind::PlanFirst));

        let kinds = kinds_of(&[row(3, "改完之后跑测试")]);
        assert!(kinds.contains(&SignalKind::TestRequirement));

        let kinds = kinds_of(&[row(4, "Run the tests after the change please")]);
        assert!(kinds.contains(&SignalKind::TestRequirement));
    }

    #[test]
    fn detects_correction_keywords_and_records_excerpt() {
        let signals = detect_signals(&[row(1, "不对，不是这个意思，重新来")]);
        let correction = signals
            .iter()
            .find(|s| s.kind == SignalKind::Correction)
            .expect("correction signal");
        assert_eq!(correction.event_id, 1);
        assert_eq!(correction.excerpt, "不对，不是这个意思，重新来");

        let kinds = kinds_of(&[row(2, "That's wrong, please start over here")]);
        assert!(kinds.contains(&SignalKind::Correction));
    }

    #[test]
    fn each_tool_name_is_one_evidence_unit() {
        let mut r = row(1, "run it now for me please");
        r.tool_names = vec!["bash".to_string(), "bash".to_string(), String::new()];
        let kinds = kinds_of(std::slice::from_ref(&r));
        let bash_count = kinds
            .iter()
            .filter(|k| matches!(k, SignalKind::ToolUsage { tool_name } if tool_name == "bash"))
            .count();
        assert_eq!(bash_count, 2, "each call is one evidence unit");
        // Empty names never become signals.
        assert_eq!(
            kinds
                .iter()
                .filter(|k| matches!(k, SignalKind::ToolUsage { .. }))
                .count(),
            2
        );
    }

    #[test]
    fn rapid_followup_in_same_conversation_is_a_correction() {
        let mut a = row(1, "implement feature x for me please");
        let mut b = row(2, "stop, use the other module instead");
        a.timestamp_ns = Some(1_000_000_000);
        b.timestamp_ns = Some(10_000_000_000); // 9s later, inside the window
        let detected = detect_signals(&[a.clone(), b.clone()]);
        let corrections: Vec<_> = detected
            .iter()
            .filter(|s| s.kind == SignalKind::Correction)
            .collect();
        assert_eq!(corrections.len(), 1);
        assert_eq!(corrections[0].event_id, 2, "attributed to the later event");

        // Outside the window: no correction.
        b.timestamp_ns = Some(1_000_000_000 + CORRECTION_WINDOW_NS + 1);
        let detected = detect_signals(&[a.clone(), b.clone()]);
        assert!(!detected.iter().any(|s| s.kind == SignalKind::Correction));

        // Same query re-sent (agent's follow-up calls): no correction.
        b.timestamp_ns = Some(2_000_000_000);
        b.user_text = a.user_text.clone();
        let detected = detect_signals(&[a.clone(), b.clone()]);
        assert!(!detected.iter().any(|s| s.kind == SignalKind::Correction));

        // Different conversations: no correction.
        let mut c = row(3, "now do something entirely different");
        c.conversation_id = Some("c2".to_string());
        c.timestamp_ns = Some(3_000_000_000);
        let detected = detect_signals(&[a, c]);
        assert!(!detected.iter().any(|s| s.kind == SignalKind::Correction));
    }

    #[test]
    fn rows_without_timestamps_never_trigger_the_followup_rule() {
        // Two clearly different queries in one conversation, but neither
        // carries a reliable timestamp: the time-gap rule must stay silent
        // rather than assume adjacency in time.
        let mut a = row(1, "implement feature x for me please");
        let mut b = row(2, "stop, use the other module instead");
        a.timestamp_ns = None;
        b.timestamp_ns = None;
        let detected = detect_signals(&[a, b]);
        assert!(!detected.iter().any(|s| s.kind == SignalKind::Correction));
    }
}
