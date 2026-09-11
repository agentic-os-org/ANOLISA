//! The seven per-category section renderers.
//!
//! Migrated from the `_summarize_*` functions of v1
//! `security_events/summary_formatter.py`, plus
//! `_skill_ledger_latest_statuses`. Each renderer receives its category's events
//! already sorted newest-first and returns one section without a trailing
//! newline; the caller joins sections with a blank line.

use std::collections::BTreeMap;

use asc_security_events::{EventResult, SecurityEvent};
use serde_json::Value;

use crate::details::{
    VerifyOutcome, counter_key, has_hardening_stats, int_field, is_truthy, mode_of, request_of,
    result_of, text, verify_outcome,
};
use crate::time_fmt::format_timestamp;

/// How many alert lines each capped list prints.
const ALERT_LIMIT: usize = 3;

/// Counts of one key across a set of events, iterated in key order.
///
/// v1 sorts the `defaultdict` items before rendering, so an ordered map is the
/// faithful carrier — an insertion-ordered one would leak input order into the
/// output.
type Counts = BTreeMap<String, u64>;

fn succeeded(event: &SecurityEvent) -> bool {
    event.result == EventResult::Succeeded
}

fn render_counts(counts: &Counts) -> String {
    counts
        .iter()
        .map(|(key, count)| format!("{key}: {count}"))
        .collect::<Vec<_>>()
        .join(", ")
}

/// Renders `map[key]` as v1's f-string would, defaulting to `0`.
fn number_field(map: &serde_json::Map<String, Value>, key: &str) -> String {
    map.get(key).map_or_else(|| "0".to_owned(), text)
}

fn is_truthy_field(map: &serde_json::Map<String, Value>, key: &str) -> bool {
    map.get(key).is_some_and(is_truthy)
}

/// Renders the `hardening` section.
pub(crate) fn hardening(events: &[&SecurityEvent]) -> String {
    let mut lines = vec!["--- Hardening ---".to_owned()];

    let mut scans: Vec<&SecurityEvent> = Vec::new();
    let mut reinforcements: Vec<&SecurityEvent> = Vec::new();
    for event in events {
        match mode_of(event).as_str() {
            "scan" => scans.push(event),
            "reinforce" => reinforcements.push(event),
            _ => {}
        }
    }

    let scans_ok = scans.iter().filter(|event| succeeded(event)).count();
    lines.push(format!(
        "  Scans performed:  {} (succeeded: {scans_ok}, failed: {})",
        scans.len(),
        scans.len() - scans_ok
    ));

    if !reinforcements.is_empty() {
        let ok = reinforcements
            .iter()
            .filter(|event| succeeded(event))
            .count();
        lines.push(format!(
            "  Reinforcements:   {} (succeeded: {ok}, failed: {})",
            reinforcements.len(),
            reinforcements.len() - ok
        ));
    }

    // Loongshield exits non-zero for a non-compliant scan, so the newest scan
    // with parsed statistics matters more than the newest scan overall.
    if let Some(latest) = scans
        .iter()
        .copied()
        .find(|event| has_hardening_stats(event))
    {
        lines.extend(latest_scan_result(latest, &reinforcements));
    } else if let Some(latest) = scans.first() {
        let error = latest
            .details
            .get("error")
            .map_or_else(|| "unknown error".to_owned(), text);
        lines.push(String::new());
        lines.push(format!("  Latest scan failed: {error}"));
    }

    lines.join("\n")
}

fn latest_scan_result(latest: &SecurityEvent, reinforcements: &[&SecurityEvent]) -> Vec<String> {
    let result = result_of(latest);
    let passed = int_field(result, "passed");
    let total = int_field(result, "total");

    // Reinforce runs repair rules, so their fixes count towards compliance.
    let fixed: i64 = reinforcements
        .iter()
        .filter(|event| has_hardening_stats(event))
        .map(|event| int_field(result_of(event), "fixed"))
        .sum();
    let effective = passed + fixed;

    if total <= 0 {
        return Vec::new();
    }
    #[expect(
        clippy::cast_precision_loss,
        reason = "rule counts are far below the f64 mantissa"
    )]
    let percent = effective as f64 / total as f64 * 100.0;

    let mut lines = vec![String::new(), "  Latest scan result:".to_owned()];
    if fixed > 0 {
        lines.push(format!(
            "    Compliance: {effective}/{total} rules passed \
             ({passed} passed + {fixed} fixed, {percent:.1}%)"
        ));
    } else {
        lines.push(format!(
            "    Compliance: {passed}/{total} rules passed ({percent:.1}%)"
        ));
    }
    if is_truthy_field(result, "failures") && fixed == 0 {
        lines.push("    Check system status using `agent-sec-cli harden --scan`".to_owned());
    }
    lines
}

/// Renders the `asset_verify` section.
pub(crate) fn asset_verify(events: &[&SecurityEvent]) -> String {
    let mut lines = vec!["--- Asset Verification ---".to_owned()];

    let outcomes: Vec<VerifyOutcome> = events.iter().map(|event| verify_outcome(event)).collect();
    let count = |wanted: VerifyOutcome| outcomes.iter().filter(|item| **item == wanted).count();
    lines.push(format!(
        "  Verifications performed: {} (verified: {}, skipped: {}, failed: {})",
        events.len(),
        count(VerifyOutcome::Verified),
        count(VerifyOutcome::NoCandidates),
        count(VerifyOutcome::Failed)
    ));

    let Some(latest) = events.first() else {
        return lines.join("\n");
    };
    let result = result_of(latest);
    let checked = result.get("checked").map_or_else(
        || (int_field(result, "passed") + int_field(result, "failed")).to_string(),
        text,
    );
    lines.push(String::new());
    lines.push("  Latest result:".to_owned());
    lines.push(format!(
        "    {checked} checked, {} passed, {} failed",
        number_field(result, "passed"),
        number_field(result, "failed")
    ));
    match verify_outcome(latest) {
        VerifyOutcome::Verified => lines.push("    Integrity status: ALL CLEAR".to_owned()),
        VerifyOutcome::NoCandidates => {
            lines.push("    Integrity status: NOT ASSESSED (no candidate skills)".to_owned());
        }
        VerifyOutcome::Failed => {
            lines.push("    Integrity status: FAILURES DETECTED".to_owned());
            if is_truthy_field(&latest.details, "error") {
                let error = latest.details.get("error").map_or_else(String::new, text);
                lines.push(format!("    Latest error: {error}"));
            }
            lines.push("    Check details using `agent-sec-cli verify`".to_owned());
        }
    }

    lines.join("\n")
}

/// Renders the `code_scan` section.
pub(crate) fn code_scan(events: &[&SecurityEvent]) -> String {
    let mut lines = vec!["--- Code Scanning ---".to_owned()];

    let mut verdicts = Counts::new();
    let mut ok = 0_usize;
    for event in events.iter().filter(|event| succeeded(event)) {
        ok += 1;
        *verdicts
            .entry(counter_key(result_of(event), "verdict", "unknown"))
            .or_default() += 1;
    }
    lines.push(format!(
        "  Scans performed: {} (succeeded: {ok}, failed: {})",
        events.len(),
        events.len() - ok
    ));
    if !verdicts.is_empty() {
        lines.push(format!("  Verdict: {}", render_counts(&verdicts)));
    }

    lines.join("\n")
}

/// Renders the `sandbox` section.
pub(crate) fn sandbox(events: &[&SecurityEvent]) -> String {
    format!(
        "--- Sandbox Guard ---\n  Total interventions: {}",
        events.len()
    )
}

/// Renders the `prompt_scan` section.
pub(crate) fn prompt_scan(events: &[&SecurityEvent]) -> String {
    let mut lines = vec!["--- Prompt Scan ---".to_owned()];

    let mut verdicts = Counts::new();
    let mut threat_types = Counts::new();
    let mut threats: Vec<&SecurityEvent> = Vec::new();
    let mut ok = 0_usize;
    for event in events.iter().filter(|event| succeeded(event)) {
        ok += 1;
        let result = result_of(event);
        *verdicts
            .entry(counter_key(result, "verdict", "unknown"))
            .or_default() += 1;
        let verdict = result.get("verdict").and_then(Value::as_str);
        if matches!(verdict, Some("warn" | "deny")) {
            *threat_types
                .entry(counter_key(result, "threat_type", "unknown"))
                .or_default() += 1;
            if threats.len() < ALERT_LIMIT {
                threats.push(event);
            }
        }
    }

    lines.push(format!(
        "  Scans performed: {} (succeeded: {ok}, failed: {})",
        events.len(),
        events.len() - ok
    ));
    if !verdicts.is_empty() {
        lines.push(format!("  Verdict breakdown: {}", render_counts(&verdicts)));
    }
    if !threat_types.is_empty() {
        lines.push(format!("  Threat types: {}", render_counts(&threat_types)));
    }
    if !threats.is_empty() {
        lines.push(String::new());
        let plural = if threats.len() > 1 { "s" } else { "" };
        lines.push(format!("  Latest threat{plural}:"));
        for event in threats {
            let result = result_of(event);
            let verdict = counter_key(result, "verdict", "?").to_uppercase();
            let threat_type = counter_key(result, "threat_type", "unknown");
            let summary = counter_key(result, "summary", "");
            let stamp = format_timestamp(&event.timestamp);
            lines.push(format!(
                "    [{stamp}] {verdict} — {threat_type}: {summary}"
            ));
        }
    }

    lines.join("\n")
}

/// Renders the `pii_scan` section.
pub(crate) fn pii_scan(events: &[&SecurityEvent]) -> String {
    let mut lines = vec!["--- PII Scan ---".to_owned()];

    let mut verdicts = Counts::new();
    let mut types = Counts::new();
    let mut ok = 0_usize;
    for event in events.iter().filter(|event| succeeded(event)) {
        ok += 1;
        let result = result_of(event);
        *verdicts
            .entry(counter_key(result, "verdict", "unknown"))
            .or_default() += 1;
        if let Some(Value::Object(summary)) = result.get("summary")
            && let Some(Value::Object(by_type)) = summary.get("by_type")
        {
            for (kind, count) in by_type {
                // Only integer counts are added; v1 skips anything else.
                if let Some(count) = count.as_u64() {
                    *types.entry(kind.clone()).or_default() += count;
                }
            }
        }
    }

    lines.push(format!(
        "  Scans performed: {} (succeeded: {ok}, failed: {})",
        events.len(),
        events.len() - ok
    ));
    if !verdicts.is_empty() {
        lines.push(format!("  Verdict breakdown: {}", render_counts(&verdicts)));
    }
    if !types.is_empty() {
        lines.push(format!("  Finding types: {}", render_counts(&types)));
    }

    lines.join("\n")
}

/// Renders the `skill_ledger` section.
pub(crate) fn skill_ledger(events: &[&SecurityEvent]) -> String {
    let mut lines = vec!["--- Skill Ledger ---".to_owned()];

    let mut checks: Vec<&SecurityEvent> = Vec::new();
    let mut certifications: Vec<&SecurityEvent> = Vec::new();
    for event in events {
        match result_of(event).get("command").and_then(Value::as_str) {
            Some("check") => checks.push(event),
            Some("certify") => certifications.push(event),
            _ => {}
        }
    }

    let checks_ok = checks.iter().filter(|event| succeeded(event)).count();
    lines.push(format!(
        "  Checks performed: {} (succeeded: {checks_ok}, failed: {})",
        checks.len(),
        checks.len() - checks_ok
    ));

    if !certifications.is_empty() {
        let mut statuses = Counts::new();
        let mut ok = 0_usize;
        for event in certifications.iter().filter(|event| succeeded(event)) {
            ok += 1;
            let result = result_of(event);
            // v1 reads `verdict` with `scanStatus` as the fallback, so a present
            // `verdict` wins even when it is null.
            let key = if result.contains_key("verdict") {
                counter_key(result, "verdict", "unknown")
            } else {
                counter_key(result, "scanStatus", "unknown")
            };
            *statuses.entry(key).or_default() += 1;
        }
        lines.push(format!(
            "  Certifications:   {ok} ({})",
            render_counts(&statuses)
        ));
    }

    let latest = latest_check_per_skill(&checks);
    if !latest.is_empty() {
        let mut statuses = Counts::new();
        let mut tampered: Vec<(String, &SecurityEvent)> = Vec::new();
        let mut denied: Vec<(String, &SecurityEvent)> = Vec::new();
        for (skill_dir, event) in &latest {
            let status = counter_key(result_of(event), "status", "unknown");
            *statuses.entry(status.clone()).or_default() += 1;
            let name = skill_name(skill_dir);
            match status.as_str() {
                "tampered" => tampered.push((name, event)),
                "deny" => denied.push((name, event)),
                _ => {}
            }
        }

        lines.push(String::new());
        lines.push(format!("  Skills tracked: {}", latest.len()));
        lines.push(format!("  Status: {}", render_counts(&statuses)));

        if !tampered.is_empty() {
            lines.push(String::new());
            lines.push(format!("  Tampered ({}):", tampered.len()));
            for (name, event) in tampered.iter().take(ALERT_LIMIT) {
                let reason = counter_key(result_of(event), "reason", "signature mismatch");
                let stamp = format_timestamp(&event.timestamp);
                lines.push(format!("    [{stamp}] {name} — {reason}"));
            }
        }
        if !denied.is_empty() {
            lines.push(String::new());
            lines.push(format!("  Denied ({}):", denied.len()));
            for (name, event) in denied.iter().take(ALERT_LIMIT) {
                let stamp = format_timestamp(&event.timestamp);
                lines.push(format!("    [{stamp}] {name} — high-risk findings"));
            }
        }
    }

    lines.join("\n")
}

/// Returns the newest succeeded check per skill directory, in first-seen order.
///
/// The events arrive newest-first, so the first hit for a directory is its latest
/// check. Order is preserved because it drives the alert lists below.
fn latest_check_per_skill<'a>(checks: &[&'a SecurityEvent]) -> Vec<(String, &'a SecurityEvent)> {
    let mut latest: Vec<(String, &SecurityEvent)> = Vec::new();
    for event in checks.iter().filter(|event| succeeded(event)) {
        let Some(skill_dir) = request_of(event).get("skill_dir").and_then(Value::as_str) else {
            continue;
        };
        if skill_dir.is_empty() || latest.iter().any(|(seen, _)| seen == skill_dir) {
            continue;
        }
        latest.push((skill_dir.to_owned(), event));
    }
    latest
}

/// Returns the trailing component of a POSIX-style skill directory.
fn skill_name(skill_dir: &str) -> String {
    skill_dir
        .trim_end_matches('/')
        .rsplit('/')
        .next()
        .unwrap_or_default()
        .to_owned()
}

/// Counts the latest status per skill, for posture and suggestions.
///
/// Same derivation as [`skill_ledger`]'s alert lists, but returned as bare counts
/// because the posture only needs the totals.
pub(crate) fn skill_ledger_latest_statuses(events: &[&SecurityEvent]) -> Counts {
    let checks: Vec<&SecurityEvent> = events
        .iter()
        .copied()
        .filter(|event| result_of(event).get("command").and_then(Value::as_str) == Some("check"))
        .collect();
    let mut counts = Counts::new();
    for (_, event) in latest_check_per_skill(&checks) {
        *counts
            .entry(counter_key(result_of(event), "status", "unknown"))
            .or_default() += 1;
    }
    counts
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

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

    fn failed(mut event: SecurityEvent) -> SecurityEvent {
        event.result = EventResult::Failed;
        event
    }

    #[test]
    fn a_hardening_section_reports_scans_and_compliance() {
        let scan = event(
            "hardening",
            "2026-01-02T03:00:00+00:00",
            json!({"result": {"mode": "scan", "passed": 8, "total": 10, "failures": [{}]}}),
        );
        let rendered = hardening(&[&scan]);
        assert_eq!(
            rendered,
            "--- Hardening ---\n  \
             Scans performed:  1 (succeeded: 1, failed: 0)\n\n  \
             Latest scan result:\n    \
             Compliance: 8/10 rules passed (80.0%)\n    \
             Check system status using `agent-sec-cli harden --scan`"
        );
    }

    #[test]
    fn reinforce_fixes_count_towards_compliance_and_hide_the_hint() {
        let scan = event(
            "hardening",
            "2026-01-02T03:00:00+00:00",
            json!({"result": {"mode": "scan", "passed": 8, "total": 10, "failures": [{}]}}),
        );
        let reinforce = event(
            "hardening",
            "2026-01-02T04:00:00+00:00",
            json!({"result": {"mode": "reinforce", "total": 10, "fixed": 2}}),
        );
        let rendered = hardening(&[&reinforce, &scan]);
        assert!(rendered.contains("  Reinforcements:   1 (succeeded: 1, failed: 0)"));
        assert!(rendered.contains("Compliance: 10/10 rules passed (8 passed + 2 fixed, 100.0%)"));
        assert!(
            !rendered.contains("harden --scan"),
            "a repaired system must not still be told to scan"
        );
    }

    /// Compliance percentages round the way Python's `f"{pct:.1f}"` does.
    ///
    /// Both sides round the exact binary value half-to-even, so `6.25` prints
    /// `6.2` while `18.75` prints `18.8`. The first five pairs land exactly on a
    /// half and therefore discriminate between tie-breaking rules; `1/8` needs no
    /// rounding at all and is the control. Verified exhaustively against
    /// `CPython` for every `total` up to 1000 and every `effective` up to
    /// `2 * total` (1,002,000 pairs, zero disagreements); these cases stay here so
    /// a future rewrite of the formatting cannot silently pick a different rule.
    #[test]
    fn compliance_percentages_round_half_to_even_like_python() {
        for (passed, total, expected) in [
            (1, 16, "6.2"),
            (3, 16, "18.8"),
            (5, 16, "31.2"),
            (11, 16, "68.8"),
            (1, 32, "3.1"),
            (1, 8, "12.5"),
        ] {
            let scan = event(
                "hardening",
                "2026-01-02T03:00:00+00:00",
                json!({"result": {"mode": "scan", "passed": passed, "total": total}}),
            );
            let rendered = hardening(&[&scan]);
            assert!(
                rendered.contains(&format!(
                    "Compliance: {passed}/{total} rules passed ({expected}%)"
                )),
                "{passed}/{total} must render as {expected}%, got:\n{rendered}"
            );
        }
    }

    #[test]
    fn a_hardening_section_without_stats_shows_the_latest_error() {
        let scan = failed(event(
            "hardening",
            "2026-01-02T03:00:00+00:00",
            json!({"result": {"mode": "scan"}, "error": "loongshield missing"}),
        ));
        let rendered = hardening(&[&scan]);
        assert!(rendered.contains("  Scans performed:  1 (succeeded: 0, failed: 1)"));
        assert!(rendered.ends_with("\n  Latest scan failed: loongshield missing"));
    }

    #[test]
    fn a_hardening_event_in_neither_mode_is_counted_nowhere() {
        let other = event(
            "hardening",
            "2026-01-02T03:00:00+00:00",
            json!({"result": {"mode": "dry-run"}}),
        );
        let rendered = hardening(&[&other]);
        assert_eq!(
            rendered,
            "--- Hardening ---\n  Scans performed:  0 (succeeded: 0, failed: 0)"
        );
    }

    #[test]
    fn an_asset_verify_section_reports_each_outcome() {
        let verified = event(
            "asset_verify",
            "2026-01-02T03:00:00+00:00",
            json!({"result": {"outcome": "verified", "passed": 3, "failed": 0, "checked": 3}}),
        );
        let skipped = event(
            "asset_verify",
            "2026-01-02T02:00:00+00:00",
            json!({"result": {"outcome": "no_candidates"}}),
        );
        let rendered = asset_verify(&[&verified, &skipped]);
        assert!(
            rendered.contains("  Verifications performed: 2 (verified: 1, skipped: 1, failed: 0)")
        );
        assert!(rendered.contains("    3 checked, 3 passed, 0 failed"));
        assert!(rendered.ends_with("    Integrity status: ALL CLEAR"));
    }

    #[test]
    fn a_failed_verification_surfaces_the_error_and_the_hint() {
        let latest = event(
            "asset_verify",
            "2026-01-02T03:00:00+00:00",
            json!({"result": {"outcome": "failed", "passed": 1, "failed": 2}, "error": "bad sig"}),
        );
        let rendered = asset_verify(&[&latest]);
        assert!(rendered.contains("    3 checked, 1 passed, 2 failed"));
        assert!(rendered.contains("    Integrity status: FAILURES DETECTED"));
        assert!(rendered.contains("    Latest error: bad sig"));
        assert!(rendered.ends_with("    Check details using `agent-sec-cli verify`"));
    }

    #[test]
    fn a_skipped_verification_is_reported_as_not_assessed() {
        let latest = event(
            "asset_verify",
            "2026-01-02T03:00:00+00:00",
            json!({"result": {}}),
        );
        let rendered = asset_verify(&[&latest]);
        assert!(rendered.contains("    0 checked, 0 passed, 0 failed"));
        assert!(rendered.ends_with("    Integrity status: NOT ASSESSED (no candidate skills)"));
    }

    #[test]
    fn a_code_scan_section_counts_verdicts_alphabetically() {
        let allow = event(
            "code_scan",
            "2026-01-02T03:00:00+00:00",
            json!({"result": {"verdict": "allow"}}),
        );
        let deny = event(
            "code_scan",
            "2026-01-02T02:00:00+00:00",
            json!({"result": {"verdict": "deny"}}),
        );
        let unknown = event("code_scan", "2026-01-02T01:00:00+00:00", json!({}));
        let broken = failed(event("code_scan", "2026-01-02T00:00:00+00:00", json!({})));
        let rendered = code_scan(&[&allow, &deny, &unknown, &broken]);
        assert_eq!(
            rendered,
            "--- Code Scanning ---\n  \
             Scans performed: 4 (succeeded: 3, failed: 1)\n  \
             Verdict: allow: 1, deny: 1, unknown: 1"
        );
    }

    #[test]
    fn a_code_scan_section_omits_the_verdict_line_when_all_failed() {
        let broken = failed(event("code_scan", "2026-01-02T00:00:00+00:00", json!({})));
        assert_eq!(
            code_scan(&[&broken]),
            "--- Code Scanning ---\n  Scans performed: 1 (succeeded: 0, failed: 1)"
        );
    }

    #[test]
    fn a_sandbox_section_is_a_single_counter() {
        let one = event("sandbox", "2026-01-02T03:00:00+00:00", json!({}));
        assert_eq!(
            sandbox(&[&one, &one]),
            "--- Sandbox Guard ---\n  Total interventions: 2"
        );
    }

    #[test]
    fn a_prompt_scan_section_caps_the_threat_list_at_three() {
        let threats: Vec<SecurityEvent> = (0..4)
            .map(|index| {
                event(
                    "prompt_scan",
                    &format!("2026-01-02T0{index}:00:00+00:00"),
                    json!({"result": {
                        "verdict": "deny",
                        "threat_type": "injection",
                        "summary": format!("s{index}"),
                    }}),
                )
            })
            .collect();
        let borrowed: Vec<&SecurityEvent> = threats.iter().collect();
        let rendered = prompt_scan(&borrowed);

        assert!(rendered.contains("  Verdict breakdown: deny: 4"));
        assert!(rendered.contains("  Threat types: injection: 4"));
        assert!(rendered.contains("  Latest threats:"));
        assert_eq!(
            rendered.matches("DENY — injection:").count(),
            3,
            "v1 caps the alert list at three"
        );
    }

    #[test]
    fn a_single_prompt_threat_uses_the_singular_heading() {
        let threat = event(
            "prompt_scan",
            "2026-01-02T03:00:00+00:00",
            json!({"result": {"verdict": "warn", "threat_type": "leak", "summary": "s"}}),
        );
        let rendered = prompt_scan(&[&threat]);
        assert!(rendered.contains("  Latest threat:"));
        assert!(rendered.contains("WARN — leak: s"));
    }

    #[test]
    fn a_clean_prompt_scan_has_no_threat_lines() {
        let clean = event(
            "prompt_scan",
            "2026-01-02T03:00:00+00:00",
            json!({"result": {"verdict": "allow"}}),
        );
        let rendered = prompt_scan(&[&clean]);
        assert_eq!(
            rendered,
            "--- Prompt Scan ---\n  \
             Scans performed: 1 (succeeded: 1, failed: 0)\n  \
             Verdict breakdown: allow: 1"
        );
    }

    #[test]
    fn a_pii_section_sums_finding_types_across_events() {
        let first = event(
            "pii_scan",
            "2026-01-02T03:00:00+00:00",
            json!({"result": {
                "verdict": "warn",
                "summary": {"by_type": {"email": 2, "phone": 1, "bogus": "x"}},
            }}),
        );
        let second = event(
            "pii_scan",
            "2026-01-02T02:00:00+00:00",
            json!({"result": {"verdict": "warn", "summary": {"by_type": {"email": 3}}}}),
        );
        let rendered = pii_scan(&[&first, &second]);
        assert!(rendered.contains("  Verdict breakdown: warn: 2"));
        assert!(
            rendered.contains("  Finding types: email: 5, phone: 1"),
            "a non-integer count is skipped, not rendered: {rendered}"
        );
    }

    #[test]
    fn a_pii_section_tolerates_a_non_object_summary() {
        let odd = event(
            "pii_scan",
            "2026-01-02T03:00:00+00:00",
            json!({"result": {"verdict": "allow", "summary": "none"}}),
        );
        let rendered = pii_scan(&[&odd]);
        assert!(!rendered.contains("Finding types"));
    }

    fn check(timestamp: &str, skill_dir: &str, status: &str) -> SecurityEvent {
        event(
            "skill_ledger",
            timestamp,
            json!({
                "result": {"command": "check", "status": status},
                "request": {"skill_dir": skill_dir},
            }),
        )
    }

    #[test]
    fn a_skill_ledger_section_deduplicates_to_the_latest_check_per_skill() {
        let newest = check("2026-01-02T03:00:00+00:00", "/s/a", "pass");
        let older = check("2026-01-02T01:00:00+00:00", "/s/a", "tampered");
        let other = check("2026-01-02T02:00:00+00:00", "/s/b", "tampered");

        let rendered = skill_ledger(&[&newest, &other, &older]);
        assert!(rendered.contains("  Checks performed: 3 (succeeded: 3, failed: 0)"));
        assert!(rendered.contains("  Skills tracked: 2"));
        assert!(rendered.contains("  Status: pass: 1, tampered: 1"));
        assert!(rendered.contains("  Tampered (1):"));
        assert!(rendered.contains("] b — signature mismatch"));
    }

    #[test]
    fn a_denied_skill_gets_its_own_block() {
        let denied = check("2026-01-02T03:00:00+00:00", "skills/risky/", "deny");
        let rendered = skill_ledger(&[&denied]);
        assert!(rendered.contains("  Denied (1):"));
        assert!(
            rendered.contains("] risky — high-risk findings"),
            "a trailing slash must not swallow the skill name: {rendered}"
        );
    }

    #[test]
    fn certifications_report_the_verdict_with_a_scan_status_fallback() {
        let with_verdict = event(
            "skill_ledger",
            "2026-01-02T03:00:00+00:00",
            json!({"result": {"command": "certify", "verdict": "clean"}}),
        );
        let legacy = event(
            "skill_ledger",
            "2026-01-02T02:00:00+00:00",
            json!({"result": {"command": "certify", "scanStatus": "dirty"}}),
        );
        let rendered = skill_ledger(&[&with_verdict, &legacy]);
        assert!(rendered.contains("  Certifications:   2 (clean: 1, dirty: 1)"));
    }

    #[test]
    fn a_check_without_a_skill_directory_is_not_tracked() {
        let anonymous = event(
            "skill_ledger",
            "2026-01-02T03:00:00+00:00",
            json!({"result": {"command": "check", "status": "pass"}}),
        );
        let rendered = skill_ledger(&[&anonymous]);
        assert!(rendered.contains("  Checks performed: 1 (succeeded: 1, failed: 0)"));
        assert!(!rendered.contains("Skills tracked"));
    }

    #[test]
    fn the_latest_statuses_helper_matches_the_section() {
        let newest = check("2026-01-02T03:00:00+00:00", "/s/a", "pass");
        let older = check("2026-01-02T01:00:00+00:00", "/s/a", "tampered");
        let counts = skill_ledger_latest_statuses(&[&newest, &older]);
        assert_eq!(counts.get("pass"), Some(&1));
        assert_eq!(counts.get("tampered"), None);
    }

    #[test]
    fn a_failed_check_never_contributes_a_status() {
        let broken = failed(check("2026-01-02T03:00:00+00:00", "/s/a", "tampered"));
        assert!(skill_ledger_latest_statuses(&[&broken]).is_empty());
    }
}
