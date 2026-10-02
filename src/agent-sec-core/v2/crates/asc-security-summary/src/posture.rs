//! Overall posture header, footer stats and suggested actions.
//!
//! Migrated from v1 `_compute_posture`, `_build_footer` and
//! `_compute_suggestions`.

use std::collections::BTreeMap;

use asc_security_events::{EventResult, SecurityEvent};
use chrono::{DateTime, Utc};

use crate::details::{
    VerifyOutcome, has_actionable_hardening_failure, has_hardening_stats, is_full_verify,
    is_truthy, result_of, verify_outcome,
};
use crate::time_fmt::time_since;

/// The events that feed the posture verdict, all newest-first.
#[derive(Debug, Default, Clone, Copy)]
pub(crate) struct PostureInput<'a> {
    /// `hardening` events.
    pub hardening: &'a [&'a SecurityEvent],
    /// `asset_verify` events.
    pub asset_verify: &'a [&'a SecurityEvent],
    /// `prompt_scan` events.
    pub prompt_scan: &'a [&'a SecurityEvent],
    /// `pii_scan` events.
    pub pii_scan: &'a [&'a SecurityEvent],
}

/// Renders the header, whose last line is the overall verdict.
pub(crate) fn header(
    input: PostureInput<'_>,
    ledger_statuses: &BTreeMap<String, u64>,
    time_label: &str,
) -> String {
    let status = if needs_attention(input, ledger_statuses) {
        "System Status: Needs attention \u{26a0}"
    } else {
        "System Status: Good \u{2713}"
    };
    format!("Security Posture Summary ({time_label})\n\n{status}")
}

fn needs_attention(input: PostureInput<'_>, ledger_statuses: &BTreeMap<String, u64>) -> bool {
    // Hardening: only the newest event counts, and a clean exit code with parsed
    // failures still needs attention.
    if let Some(latest) = input.hardening.first() {
        match latest.result {
            EventResult::Failed => return true,
            EventResult::Succeeded => {
                if result_of(latest).get("failures").is_some_and(is_truthy) {
                    return true;
                }
            }
        }
    }

    // Asset verification: only a conclusive *full* run may change the verdict, so
    // a single-skill run or a no-candidate run leaves the prior status standing.
    let conclusive_full = input.asset_verify.iter().copied().find(|event| {
        is_full_verify(event) && verify_outcome(event) != VerifyOutcome::NoCandidates
    });
    if conclusive_full.is_some_and(|event| verify_outcome(event) == VerifyOutcome::Failed) {
        return true;
    }

    if any_denied(input.prompt_scan) || any_denied(input.pii_scan) {
        return true;
    }

    ledger_statuses.get("tampered").copied().unwrap_or(0) > 0
        || ledger_statuses.get("deny").copied().unwrap_or(0) > 0
}

fn any_denied(events: &[&SecurityEvent]) -> bool {
    events.iter().any(|event| {
        event.result == EventResult::Succeeded
            && result_of(event)
                .get("verdict")
                .and_then(|value| value.as_str())
                == Some("deny")
    })
}

/// Renders the footer: totals, the age of the newest event, and suggestions.
pub(crate) fn footer(
    events: &[SecurityEvent],
    hardening: &[&SecurityEvent],
    ledger_statuses: &BTreeMap<String, u64>,
    now: DateTime<Utc>,
) -> String {
    let failed = events
        .iter()
        .filter(|event| event.result == EventResult::Failed)
        .count();
    let last = newest(events).map_or_else(
        || "N/A".to_owned(),
        |event| time_since(&event.timestamp, now),
    );

    let mut lines = vec![
        "---".to_owned(),
        format!(
            "Total events: {}  |  Failed: {failed}  |  Last event: {last}",
            events.len()
        ),
    ];

    let suggestions = suggestions(hardening, ledger_statuses);
    if !suggestions.is_empty() {
        lines.push(String::new());
        lines.push("Suggested actions:".to_owned());
        lines.extend(suggestions.into_iter().map(|item| format!("  {item}")));
    }

    lines.join("\n")
}

/// Returns the newest event, preferring the earliest on a tie.
///
/// Python's `max` keeps the first maximal element while Rust's `max_by_key` keeps
/// the last, so the comparison is written out to preserve v1's choice.
fn newest(events: &[SecurityEvent]) -> Option<&SecurityEvent> {
    events
        .iter()
        .fold(None, |best: Option<&SecurityEvent>, event| match best {
            Some(current) if current.timestamp >= event.timestamp => Some(current),
            _ => Some(event),
        })
}

/// Hints keyed by ledger status, in v1's fixed emission order.
const LEDGER_HINTS: &[(&str, &str)] = &[
    (
        "tampered",
        "agent-sec-cli skill-ledger check <dir>    Investigate tampered skills",
    ),
    (
        "drifted",
        "agent-sec-cli skill-ledger scan <dir>     Re-scan drifted skills",
    ),
    (
        "none",
        "agent-sec-cli skill-ledger scan <dir>     Scan unchecked skills",
    ),
];

fn suggestions(
    hardening: &[&SecurityEvent],
    ledger_statuses: &BTreeMap<String, u64>,
) -> Vec<&'static str> {
    let mut suggestions = Vec::new();

    if let Some(latest) = hardening.first()
        && has_actionable_hardening_failure(latest)
        && (latest.result == EventResult::Succeeded || has_hardening_stats(latest))
    {
        suggestions.push("agent-sec-cli harden --reinforce    Fix failed rules");
    }

    for (status, hint) in LEDGER_HINTS {
        if ledger_statuses.get(*status).copied().unwrap_or(0) > 0 {
            suggestions.push(hint);
        }
    }

    suggestions
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use serde_json::{Value, json};

    fn event(category: &str, timestamp: &str, details: Value) -> SecurityEvent {
        let Value::Object(details) = details else {
            unreachable!("test fixtures always pass an object")
        };
        let mut event = SecurityEvent::new("t", category, details);
        event
            .set_timestamp(timestamp)
            .expect("the fixture timestamps are valid");
        event
    }

    fn now() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 1, 2, 12, 0, 0)
            .single()
            .expect("timestamp")
    }

    fn statuses(pairs: &[(&str, u64)]) -> BTreeMap<String, u64> {
        pairs
            .iter()
            .map(|(key, count)| ((*key).to_owned(), *count))
            .collect()
    }

    #[test]
    fn a_quiet_window_reads_as_good() {
        let rendered = header(PostureInput::default(), &BTreeMap::new(), "last 24 hours");
        assert_eq!(
            rendered,
            "Security Posture Summary (last 24 hours)\n\nSystem Status: Good \u{2713}"
        );
    }

    #[test]
    fn a_failed_hardening_run_needs_attention() {
        let mut latest = event("hardening", "2026-01-02T03:00:00+00:00", json!({}));
        latest.result = EventResult::Failed;
        let hardening = [&latest];
        let input = PostureInput {
            hardening: &hardening,
            ..PostureInput::default()
        };
        assert!(header(input, &BTreeMap::new(), "t").ends_with("Needs attention \u{26a0}"));
    }

    #[test]
    fn a_succeeded_hardening_run_with_failures_still_needs_attention() {
        let latest = event(
            "hardening",
            "2026-01-02T03:00:00+00:00",
            json!({"result": {"failures": [{"rule_id": "R-1"}]}}),
        );
        let hardening = [&latest];
        let input = PostureInput {
            hardening: &hardening,
            ..PostureInput::default()
        };
        assert!(header(input, &BTreeMap::new(), "t").ends_with("Needs attention \u{26a0}"));
    }

    #[test]
    fn only_the_newest_hardening_run_counts() {
        let newest = event("hardening", "2026-01-02T03:00:00+00:00", json!({}));
        let older = event(
            "hardening",
            "2026-01-02T01:00:00+00:00",
            json!({"result": {"failures": [{"rule_id": "R-1"}]}}),
        );
        let hardening = [&newest, &older];
        let input = PostureInput {
            hardening: &hardening,
            ..PostureInput::default()
        };
        assert!(header(input, &BTreeMap::new(), "t").ends_with("Good \u{2713}"));
    }

    #[test]
    fn a_single_skill_verification_never_changes_the_posture() {
        let single = event(
            "asset_verify",
            "2026-01-02T03:00:00+00:00",
            json!({"result": {"outcome": "failed"}, "request": {"skill": "a"}}),
        );
        let asset_verify = [&single];
        let input = PostureInput {
            asset_verify: &asset_verify,
            ..PostureInput::default()
        };
        assert!(header(input, &BTreeMap::new(), "t").ends_with("Good \u{2713}"));
    }

    #[test]
    fn a_no_candidate_run_is_skipped_in_favour_of_the_next_conclusive_one() {
        let skipped = event(
            "asset_verify",
            "2026-01-02T03:00:00+00:00",
            json!({"result": {"outcome": "no_candidates"}}),
        );
        let failed = event(
            "asset_verify",
            "2026-01-02T02:00:00+00:00",
            json!({"result": {"outcome": "failed"}}),
        );
        let asset_verify = [&skipped, &failed];
        let input = PostureInput {
            asset_verify: &asset_verify,
            ..PostureInput::default()
        };
        assert!(header(input, &BTreeMap::new(), "t").ends_with("Needs attention \u{26a0}"));
    }

    #[test]
    fn a_denied_scan_of_either_kind_needs_attention() {
        let denied = event(
            "prompt_scan",
            "2026-01-02T03:00:00+00:00",
            json!({"result": {"verdict": "deny"}}),
        );
        let prompt = [&denied];
        let input = PostureInput {
            prompt_scan: &prompt,
            ..PostureInput::default()
        };
        assert!(header(input, &BTreeMap::new(), "t").ends_with("Needs attention \u{26a0}"));

        let pii = [&denied];
        let input = PostureInput {
            pii_scan: &pii,
            ..PostureInput::default()
        };
        assert!(header(input, &BTreeMap::new(), "t").ends_with("Needs attention \u{26a0}"));
    }

    /// A scan that failed cannot raise the posture, whatever its verdict says.
    ///
    /// v1 requires `result == "succeeded"` before trusting the verdict: a scan
    /// that crashed may have written `deny` while never finishing, and treating
    /// that as a real detection would turn every scanner bug into an alert.
    #[test]
    fn a_failed_scan_does_not_raise_even_with_a_deny_verdict() {
        let mut denied = event(
            "prompt_scan",
            "2026-01-02T03:00:00+00:00",
            json!({"result": {"verdict": "deny"}}),
        );
        denied.result = EventResult::Failed;
        let prompt = [&denied];
        let input = PostureInput {
            prompt_scan: &prompt,
            ..PostureInput::default()
        };
        assert!(header(input, &BTreeMap::new(), "t").ends_with("Good \u{2713}"));
    }

    #[test]
    fn a_warned_scan_does_not_need_attention() {
        let warned = event(
            "prompt_scan",
            "2026-01-02T03:00:00+00:00",
            json!({"result": {"verdict": "warn"}}),
        );
        let prompt = [&warned];
        let input = PostureInput {
            prompt_scan: &prompt,
            ..PostureInput::default()
        };
        assert!(header(input, &BTreeMap::new(), "t").ends_with("Good \u{2713}"));
    }

    #[test]
    fn a_tampered_or_denied_skill_needs_attention() {
        for status in ["tampered", "deny"] {
            let rendered = header(PostureInput::default(), &statuses(&[(status, 1)]), "t");
            assert!(rendered.ends_with("Needs attention \u{26a0}"), "{status}");
        }
        let rendered = header(PostureInput::default(), &statuses(&[("pass", 5)]), "t");
        assert!(rendered.ends_with("Good \u{2713}"));
    }

    #[test]
    fn a_footer_reports_totals_and_the_newest_age() {
        let older = event("code_scan", "2026-01-02T10:00:00+00:00", json!({}));
        let mut broken = event("code_scan", "2026-01-02T11:00:00+00:00", json!({}));
        broken.result = EventResult::Failed;
        let events = vec![older, broken];

        let rendered = footer(&events, &[], &BTreeMap::new(), now());
        assert_eq!(
            rendered,
            "---\nTotal events: 2  |  Failed: 1  |  Last event: 1h ago"
        );
    }

    #[test]
    fn an_empty_event_list_has_no_last_event() {
        let rendered = footer(&[], &[], &BTreeMap::new(), now());
        assert!(rendered.ends_with("Last event: N/A"));
    }

    #[test]
    fn suggestions_are_emitted_in_a_fixed_order() {
        let latest = event(
            "hardening",
            "2026-01-02T03:00:00+00:00",
            json!({"result": {"total": 3, "failures": [{"rule_id": "R-1"}]}}),
        );
        let hardening = [&latest];
        let rendered = footer(
            &[],
            &hardening,
            &statuses(&[("none", 1), ("drifted", 2), ("tampered", 1)]),
            now(),
        );
        let lines: Vec<&str> = rendered.lines().collect();
        assert_eq!(lines[2], "");
        assert_eq!(lines[3], "Suggested actions:");
        assert_eq!(
            &lines[4..],
            [
                "  agent-sec-cli harden --reinforce    Fix failed rules",
                "  agent-sec-cli skill-ledger check <dir>    Investigate tampered skills",
                "  agent-sec-cli skill-ledger scan <dir>     Re-scan drifted skills",
                "  agent-sec-cli skill-ledger scan <dir>     Scan unchecked skills",
            ]
        );
    }

    #[test]
    fn a_failed_hardening_run_without_stats_suggests_nothing() {
        let mut latest = event(
            "hardening",
            "2026-01-02T03:00:00+00:00",
            json!({"result": {"failures": [{"rule_id": "R-1"}]}}),
        );
        latest.result = EventResult::Failed;
        let hardening = [&latest];
        let rendered = footer(&[], &hardening, &BTreeMap::new(), now());
        assert!(
            !rendered.contains("Suggested actions"),
            "without parsed statistics there is nothing to reinforce"
        );
    }

    #[test]
    fn a_passing_ledger_suggests_nothing() {
        let rendered = footer(&[], &[], &statuses(&[("pass", 3)]), now());
        assert!(!rendered.contains("Suggested actions"));
    }
}
