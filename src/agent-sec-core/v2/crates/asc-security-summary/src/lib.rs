//! Human-readable security posture summary rendered from [`SecurityEvent`]s.
//!
//! Migrated from v1 `security_events/summary_formatter.py`. The output is a
//! header, one section per non-empty category in a fixed order, and a footer,
//! joined by blank lines.
//!
//! # Determinism
//!
//! The footer prints the age of the newest event, so the wall clock is an input.
//! [`format_summary_at`] takes it explicitly, which is what makes byte-for-byte
//! comparison against v1 possible; [`format_summary`] is the `Utc::now()`
//! convenience wrapper.

#![forbid(unsafe_code)]

mod details;
mod posture;
mod sections;
mod time_fmt;

use asc_security_events::SecurityEvent;
use chrono::{DateTime, Utc};

use crate::posture::PostureInput;

/// The text returned when there is nothing to report.
///
/// This is the one output that ends in a newline; every other rendering ends on
/// the last footer line, exactly as v1 does.
pub const NO_EVENTS: &str = "No security events recorded.\n";

/// Renders the summary for `events` over the window described by `time_label`.
///
/// `events` need not be sorted; each category is ordered newest-first internally.
#[must_use]
pub fn format_summary(events: &[SecurityEvent], time_label: &str) -> String {
    format_summary_at(events, time_label, Utc::now())
}

/// Renders the summary as of `now`.
///
/// See the module documentation for why `now` is injected.
#[must_use]
pub fn format_summary_at(events: &[SecurityEvent], time_label: &str, now: DateTime<Utc>) -> String {
    if events.is_empty() {
        return NO_EVENTS.to_owned();
    }

    let hardening = group(events, "hardening");
    let asset_verify = group(events, "asset_verify");
    let code_scan = group(events, "code_scan");
    let sandbox = group(events, "sandbox");
    let prompt_scan = group(events, "prompt_scan");
    let pii_scan = group(events, "pii_scan");
    let skill_ledger = group(events, "skill_ledger");

    // Section order is part of the contract, so it is spelled out rather than
    // derived from whichever categories happen to be present.
    let rendered = [
        (
            &hardening,
            sections::hardening as fn(&[&SecurityEvent]) -> String,
        ),
        (&asset_verify, sections::asset_verify),
        (&code_scan, sections::code_scan),
        (&sandbox, sections::sandbox),
        (&prompt_scan, sections::prompt_scan),
        (&pii_scan, sections::pii_scan),
        (&skill_ledger, sections::skill_ledger),
    ];

    let ledger_statuses = sections::skill_ledger_latest_statuses(&skill_ledger);
    let mut parts = vec![posture::header(
        PostureInput {
            hardening: &hardening,
            asset_verify: &asset_verify,
            prompt_scan: &prompt_scan,
            pii_scan: &pii_scan,
        },
        &ledger_statuses,
        time_label,
    )];
    for (group, render) in rendered {
        if !group.is_empty() {
            parts.push(render(group));
        }
    }
    parts.push(posture::footer(events, &hardening, &ledger_statuses, now));

    parts.join("\n\n")
}

/// Returns the events of one category, newest-first.
///
/// The sort is stable, so events sharing a timestamp keep their input order —
/// v1's `list.sort(reverse=True)` behaves the same way and the tie order is
/// visible in the capped alert lists.
fn group<'a>(events: &'a [SecurityEvent], category: &str) -> Vec<&'a SecurityEvent> {
    let mut picked: Vec<&SecurityEvent> = events
        .iter()
        .filter(|event| event.category == category)
        .collect();
    picked.sort_by(|left, right| right.timestamp.cmp(&left.timestamp));
    picked
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use serde_json::{Map, Value, json};

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

    #[test]
    fn an_empty_event_list_renders_the_placeholder() {
        assert_eq!(format_summary(&[], "last 24 hours"), NO_EVENTS);
    }

    /// A denied code scan is reported in full but leaves the posture alone.
    ///
    /// v1 draws the line here deliberately: a code scan that denied means the
    /// gate worked, while a denied *prompt* scan means untrusted input reached
    /// the agent. Only the four categories in `PostureInput` can raise the
    /// header, and `code_scan` is not one of them.
    #[test]
    fn a_denied_code_scan_is_reported_without_raising_the_header() {
        let events = vec![event(
            "code_scan",
            "2026-01-02T11:00:00+00:00",
            json!({"result": {"verdict": "deny"}}),
        )];
        let rendered = format_summary_at(&events, "last 24 hours", now());

        assert!(rendered.contains("System Status: Good \u{2713}"));
        assert!(
            rendered.contains("deny: 1"),
            "the verdict must still be visible: {rendered}"
        );
    }

    #[test]
    fn a_single_category_renders_header_section_and_footer() {
        let events = vec![event(
            "sandbox",
            "2026-01-02T11:00:00+00:00",
            Value::Object(Map::new()),
        )];
        let rendered = format_summary_at(&events, "last 24 hours", now());
        assert_eq!(
            rendered,
            "Security Posture Summary (last 24 hours)\n\n\
             System Status: Good \u{2713}\n\n\
             --- Sandbox Guard ---\n  \
             Total interventions: 1\n\n\
             ---\n\
             Total events: 1  |  Failed: 0  |  Last event: 1h ago"
        );
        assert!(
            !rendered.ends_with('\n'),
            "only the empty rendering ends in a newline"
        );
    }

    #[test]
    fn sections_follow_the_declared_order_regardless_of_input_order() {
        let events = vec![
            event("skill_ledger", "2026-01-02T01:00:00+00:00", json!({})),
            event("pii_scan", "2026-01-02T02:00:00+00:00", json!({})),
            event("sandbox", "2026-01-02T03:00:00+00:00", json!({})),
            event("code_scan", "2026-01-02T04:00:00+00:00", json!({})),
            event("prompt_scan", "2026-01-02T05:00:00+00:00", json!({})),
            event("asset_verify", "2026-01-02T06:00:00+00:00", json!({})),
            event("hardening", "2026-01-02T07:00:00+00:00", json!({})),
        ];
        let rendered = format_summary_at(&events, "t", now());
        let headings: Vec<&str> = rendered
            .lines()
            .filter(|line| line.starts_with("--- "))
            .collect();
        assert_eq!(
            headings,
            [
                "--- Hardening ---",
                "--- Asset Verification ---",
                "--- Code Scanning ---",
                "--- Sandbox Guard ---",
                "--- Prompt Scan ---",
                "--- PII Scan ---",
                "--- Skill Ledger ---",
            ]
        );
    }

    #[test]
    fn an_unknown_category_contributes_only_to_the_footer() {
        let events = vec![event("mystery", "2026-01-02T11:00:00+00:00", json!({}))];
        let rendered = format_summary_at(&events, "t", now());
        assert!(!rendered.contains("--- "));
        assert!(rendered.contains("Total events: 1"));
    }

    #[test]
    fn each_category_is_ordered_newest_first() {
        let events = vec![
            event(
                "prompt_scan",
                "2026-01-02T01:00:00+00:00",
                json!({"result": {"verdict": "deny", "threat_type": "old", "summary": "s"}}),
            ),
            event(
                "prompt_scan",
                "2026-01-02T09:00:00+00:00",
                json!({"result": {"verdict": "deny", "threat_type": "new", "summary": "s"}}),
            ),
        ];
        let rendered = format_summary_at(&events, "t", now());
        let new_at = rendered.find("new").expect("the newest threat is listed");
        let old_at = rendered.find("old").expect("the oldest threat is listed");
        assert!(new_at < old_at, "the newest event must be listed first");
    }

    #[test]
    fn the_injected_clock_is_the_only_source_of_the_footer_age() {
        let events = vec![event("sandbox", "2026-01-02T11:00:00+00:00", json!({}))];
        let earlier = format_summary_at(&events, "t", now());
        let later = format_summary_at(
            &events,
            "t",
            Utc.with_ymd_and_hms(2026, 1, 3, 12, 0, 0)
                .single()
                .expect("timestamp"),
        );
        assert!(earlier.contains("Last event: 1h ago"));
        assert!(later.contains("Last event: 1d ago"));
    }

    #[test]
    fn a_tampered_skill_drives_both_the_header_and_the_suggestions() {
        let events = vec![event(
            "skill_ledger",
            "2026-01-02T11:00:00+00:00",
            json!({
                "result": {"command": "check", "status": "tampered"},
                "request": {"skill_dir": "/s/a"},
            }),
        )];
        let rendered = format_summary_at(&events, "t", now());
        assert!(rendered.contains("System Status: Needs attention \u{26a0}"));
        assert!(rendered.contains("  Tampered (1):"));
        assert!(rendered.contains("Investigate tampered skills"));
    }
}
