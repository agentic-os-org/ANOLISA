//! The human-readable security posture summary, ported from v1
//! `security_events/summary_formatter.py` shape for shape.
//!
//! The summary is a *CLI projection*: it consumes the event payloads the
//! daemon already returned (details included) and renders the same
//! multi-section text v1 produced — the posture header, one section per
//! present category, and the footer with counts and suggested actions.

use serde_json::{Map, Value};

/// Produces the v1 summary text for `events` (newest-first from the daemon).
#[must_use]
pub fn format_summary(events: &[Value], time_label: &str) -> String {
    if events.is_empty() {
        return "No security events recorded.\n".to_owned();
    }

    let by_category = group_by_category(events);
    let mut sections: Vec<String> = Vec::new();

    let hardening = by_category.get("hardening").cloned().unwrap_or_default();
    let asset_verify = by_category.get("asset_verify").cloned().unwrap_or_default();
    let code_scan = by_category.get("code_scan").cloned().unwrap_or_default();
    let sandbox = by_category.get("sandbox").cloned().unwrap_or_default();
    let prompt_scan = by_category.get("prompt_scan").cloned().unwrap_or_default();
    let pii_scan = by_category.get("pii_scan").cloned().unwrap_or_default();
    let skill_ledger = by_category.get("skill_ledger").cloned().unwrap_or_default();

    if !hardening.is_empty() {
        sections.push(summarize_hardening(&hardening));
    }
    if !asset_verify.is_empty() {
        sections.push(summarize_asset_verify(&asset_verify));
    }
    if !code_scan.is_empty() {
        sections.push(summarize_code_scan(&code_scan));
    }
    if !sandbox.is_empty() {
        sections.push(summarize_sandbox(&sandbox));
    }
    if !prompt_scan.is_empty() {
        sections.push(summarize_prompt_scan(&prompt_scan));
    }
    if !pii_scan.is_empty() {
        sections.push(summarize_pii_scan(&pii_scan));
    }
    if !skill_ledger.is_empty() {
        sections.push(summarize_skill_ledger(&skill_ledger));
    }

    let ledger_statuses = if skill_ledger.is_empty() {
        std::collections::BTreeMap::new()
    } else {
        skill_ledger_latest_statuses(&skill_ledger)
    };

    let header = compute_posture(
        &hardening,
        &asset_verify,
        &prompt_scan,
        &pii_scan,
        &ledger_statuses,
        time_label,
    );
    let footer = build_footer(events, &hardening, &ledger_statuses);
    let mut parts = vec![header];
    parts.extend(sections);
    parts.push(footer);
    parts.join("\n\n")
}

/// Groups events by category, each group newest-first, like v1.
fn group_by_category(events: &[Value]) -> std::collections::BTreeMap<String, Vec<Value>> {
    let mut by_category: std::collections::BTreeMap<String, Vec<Value>> =
        std::collections::BTreeMap::new();
    for event in events {
        let category = str_of(event, "category").to_owned();
        by_category.entry(category).or_default().push(event.clone());
    }
    for group in by_category.values_mut() {
        group.sort_by(|left, right| str_of(right, "timestamp").cmp(str_of(left, "timestamp")));
    }
    by_category
}

fn str_of<'a>(event: &'a Value, field: &str) -> &'a str {
    event.get(field).and_then(Value::as_str).unwrap_or_default()
}

/// `details.result`, or an empty object.
fn result_of(event: &Value) -> Value {
    event
        .get("details")
        .and_then(|details| details.get("result"))
        .cloned()
        .unwrap_or_else(|| Value::Object(Map::new()))
}

/// `details.request`, or an empty object.
fn request_of(event: &Value) -> Value {
    event
        .get("details")
        .and_then(|details| details.get("request"))
        .cloned()
        .unwrap_or_else(|| Value::Object(Map::new()))
}

fn result_str(event: &Value, field: &str) -> String {
    result_of(event)
        .get(field)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned()
}

fn result_u64(event: &Value, field: &str) -> u64 {
    result_of(event)
        .get(field)
        .and_then(Value::as_u64)
        .unwrap_or_default()
}

fn request_str(event: &Value, field: &str) -> String {
    request_of(event)
        .get(field)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned()
}

fn details_str(event: &Value, field: &str) -> String {
    event
        .get("details")
        .and_then(|details| details.get(field))
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned()
}

/// Full-skill verification has `skill` absent or null in the request.
fn is_full_verify(event: &Value) -> bool {
    match request_of(event).get("skill") {
        None | Some(Value::Null) => true,
        Some(_) => false,
    }
}

/// The semantic verification outcome, including for legacy events.
fn asset_verify_outcome(event: &Value) -> &'static str {
    let outcome = result_str(event, "outcome");
    if outcome == "verified" || outcome == "failed" || outcome == "no_candidates" {
        return match outcome.as_str() {
            "verified" => "verified",
            "no_candidates" => "no_candidates",
            _ => "failed",
        };
    }
    if str_of(event, "result") == "failed" {
        return "failed";
    }
    let passed = result_u64(event, "passed");
    let failed = result_u64(event, "failed");
    if failed > 0 {
        return "failed";
    }
    if passed == 0 && failed == 0 {
        return "no_candidates";
    }
    "verified"
}

/// The hardening mode, with v1's `request.args` fallback.
fn hardening_mode(event: &Value) -> String {
    let mode = result_str(event, "mode");
    if !mode.is_empty() {
        return mode;
    }
    if let Some(args) = request_of(event).get("args").and_then(Value::as_array) {
        let words: Vec<&str> = args.iter().filter_map(Value::as_str).collect();
        if words.contains(&"--dry-run") {
            return "dry-run".to_owned();
        }
        if words.contains(&"--reinforce") {
            return "reinforce".to_owned();
        }
        if words.contains(&"--scan") {
            return "scan".to_owned();
        }
    }
    String::new()
}

fn has_hardening_stats(event: &Value) -> bool {
    result_u64(event, "total") > 0
}

fn has_actionable_hardening_failure(event: &Value) -> bool {
    let result = result_of(event);
    let Some(failures) = result.get("failures").and_then(Value::as_array) else {
        return false;
    };
    for failure in failures {
        let Some(failure) = failure.as_object() else {
            continue;
        };
        let status = failure.get("status").and_then(Value::as_str);
        if status == Some("UNKNOWN") {
            continue;
        }
        let rule = failure.get("rule_id").and_then(Value::as_str);
        if rule.unwrap_or_default().is_empty() {
            continue;
        }
        return true;
    }
    false
}

/// Renders an ISO-8601 timestamp in local time for inline display (v1
/// `_format_timestamp`).
fn format_timestamp(timestamp: &str) -> String {
    let parsed = chrono::DateTime::parse_from_rfc3339(timestamp)
        .map(|moment| moment.to_utc())
        .or_else(|_| {
            chrono::NaiveDateTime::parse_from_str(timestamp, "%Y-%m-%dT%H:%M:%S%.f")
                .map(|naive| naive.and_utc())
        })
        .or_else(|_| {
            chrono::NaiveDateTime::parse_from_str(timestamp, "%Y-%m-%d %H:%M:%S%.f")
                .map(|naive| naive.and_utc())
        });
    match parsed {
        Ok(moment) => moment
            .with_timezone(&chrono::Local)
            .format("%Y-%m-%d %H:%M:%S")
            .to_string(),
        Err(_) => timestamp.to_owned(),
    }
}

fn summarize_hardening(events: &[Value]) -> String {
    let mut lines = vec!["--- Hardening ---".to_owned()];

    let mut scans: Vec<&Value> = Vec::new();
    let mut reinforcements: Vec<&Value> = Vec::new();
    for event in events {
        match hardening_mode(event).as_str() {
            "scan" => scans.push(event),
            "reinforce" => reinforcements.push(event),
            _ => {}
        }
    }

    let scans_ok = scans
        .iter()
        .filter(|event| str_of(event, "result") == "succeeded")
        .count();
    let scans_fail = scans.len() - scans_ok;
    lines.push(format!(
        "  Scans performed:  {} (succeeded: {scans_ok}, failed: {scans_fail})",
        scans.len()
    ));

    if !reinforcements.is_empty() {
        let reinf_ok = reinforcements
            .iter()
            .filter(|event| str_of(event, "result") == "succeeded")
            .count();
        let reinf_fail = reinforcements.len() - reinf_ok;
        lines.push(format!(
            "  Reinforcements:   {} (succeeded: {reinf_ok}, failed: {reinf_fail})",
            reinforcements.len()
        ));
    }

    let latest_scan = scans
        .iter()
        .copied()
        .find(|event| has_hardening_stats(event));
    if let Some(latest_scan) = latest_scan {
        let passed = result_u64(latest_scan, "passed");
        let total = result_u64(latest_scan, "total");
        let has_failures = result_of(latest_scan)
            .get("failures")
            .is_some_and(|failures| !failures.as_array().unwrap_or(&Vec::new()).is_empty());

        let mut fixed_count = 0_u64;
        for event in &reinforcements {
            if has_hardening_stats(event) {
                fixed_count += result_u64(event, "fixed");
            }
        }
        let effective_passed = passed + fixed_count;

        if total > 0 {
            #[allow(clippy::cast_precision_loss)]
            let pct = effective_passed as f64 / total as f64 * 100.0;
            lines.push(String::new());
            lines.push("  Latest scan result:".to_owned());
            if fixed_count > 0 {
                lines.push(format!(
                    "    Compliance: {effective_passed}/{total} rules passed ({passed} passed + {fixed_count} fixed, {pct:.1}%)"
                ));
            } else {
                lines.push(format!(
                    "    Compliance: {passed}/{total} rules passed ({pct:.1}%)"
                ));
            }
            if has_failures && fixed_count == 0 {
                lines
                    .push("    Check system status using `agent-sec-cli harden --scan`".to_owned());
            }
        }
    } else if !scans.is_empty() {
        let latest_error = scans[0];
        let error_msg = if details_str(latest_error, "error").is_empty() {
            "unknown error".to_owned()
        } else {
            details_str(latest_error, "error")
        };
        lines.push(String::new());
        lines.push(format!("  Latest scan failed: {error_msg}"));
    }

    lines.join("\n")
}

fn summarize_asset_verify(events: &[Value]) -> String {
    let mut lines = vec!["--- Asset Verification ---".to_owned()];

    let outcomes: Vec<&str> = events
        .iter()
        .map(|event| asset_verify_outcome(event))
        .collect();
    let verified_count = outcomes
        .iter()
        .filter(|outcome| **outcome == "verified")
        .count();
    let skipped_count = outcomes
        .iter()
        .filter(|outcome| **outcome == "no_candidates")
        .count();
    let failed_count = outcomes
        .iter()
        .filter(|outcome| **outcome == "failed")
        .count();
    lines.push(format!(
        "  Verifications performed: {} (verified: {verified_count}, skipped: {skipped_count}, failed: {failed_count})",
        events.len()
    ));

    let latest = &events[0];
    let passed = result_u64(latest, "passed");
    let failed = result_u64(latest, "failed");
    let checked = latest
        .get("details")
        .and_then(|details| details.get("result"))
        .and_then(|result| result.get("checked"))
        .and_then(Value::as_u64)
        .unwrap_or(passed + failed);
    let outcome = asset_verify_outcome(latest);
    lines.push(String::new());
    lines.push("  Latest result:".to_owned());
    lines.push(format!(
        "    {checked} checked, {passed} passed, {failed} failed"
    ));
    if outcome == "verified" {
        lines.push("    Integrity status: ALL CLEAR".to_owned());
    } else if outcome == "no_candidates" {
        lines.push("    Integrity status: NOT ASSESSED (no candidate skills)".to_owned());
    } else {
        lines.push("    Integrity status: FAILURES DETECTED".to_owned());
        let error_msg = details_str(latest, "error");
        if !error_msg.is_empty() {
            lines.push(format!("    Latest error: {error_msg}"));
        }
        lines.push("    Check details using `agent-sec-cli verify`".to_owned());
    }

    lines.join("\n")
}

fn summarize_code_scan(events: &[Value]) -> String {
    let mut lines = vec!["--- Code Scanning ---".to_owned()];

    let mut ok_count = 0_usize;
    let mut verdict_counts: std::collections::BTreeMap<String, u64> =
        std::collections::BTreeMap::new();
    for event in events {
        if str_of(event, "result") == "succeeded" {
            ok_count += 1;
            let verdict = result_str(event, "verdict");
            let verdict = if verdict.is_empty() {
                "unknown".to_owned()
            } else {
                verdict
            };
            *verdict_counts.entry(verdict).or_insert(0) += 1;
        }
    }
    let fail_count = events.len() - ok_count;
    lines.push(format!(
        "  Scans performed: {} (succeeded: {ok_count}, failed: {fail_count})",
        events.len()
    ));

    if !verdict_counts.is_empty() {
        let parts: Vec<String> = verdict_counts
            .iter()
            .map(|(verdict, count)| format!("{verdict}: {count}"))
            .collect();
        lines.push(format!("  Verdict: {}", parts.join(", ")));
    }

    lines.join("\n")
}

fn summarize_sandbox(events: &[Value]) -> String {
    let mut lines = vec!["--- Sandbox Guard ---".to_owned()];
    lines.push(format!("  Total interventions: {}", events.len()));
    lines.join("\n")
}

fn summarize_prompt_scan(events: &[Value]) -> String {
    let mut lines = vec!["--- Prompt Scan ---".to_owned()];

    let mut ok_count = 0_usize;
    let mut verdict_counts: std::collections::BTreeMap<String, u64> =
        std::collections::BTreeMap::new();
    let mut threat_type_counts: std::collections::BTreeMap<String, u64> =
        std::collections::BTreeMap::new();
    let mut latest_threats: Vec<&Value> = Vec::new();

    for event in events {
        if str_of(event, "result") == "succeeded" {
            ok_count += 1;
            let verdict = {
                let verdict = result_str(event, "verdict");
                if verdict.is_empty() {
                    "unknown".to_owned()
                } else {
                    verdict
                }
            };
            *verdict_counts.entry(verdict.clone()).or_insert(0) += 1;
            if verdict == "warn" || verdict == "deny" {
                let threat_type = {
                    let threat = result_str(event, "threat_type");
                    if threat.is_empty() {
                        "unknown".to_owned()
                    } else {
                        threat
                    }
                };
                *threat_type_counts.entry(threat_type).or_insert(0) += 1;
                if latest_threats.len() < 3 {
                    latest_threats.push(event);
                }
            }
        }
    }
    let fail_count = events.len() - ok_count;

    lines.push(format!(
        "  Scans performed: {} (succeeded: {ok_count}, failed: {fail_count})",
        events.len()
    ));

    if !verdict_counts.is_empty() {
        let parts: Vec<String> = verdict_counts
            .iter()
            .map(|(verdict, count)| format!("{verdict}: {count}"))
            .collect();
        lines.push(format!("  Verdict breakdown: {}", parts.join(", ")));
    }

    if !threat_type_counts.is_empty() {
        let parts: Vec<String> = threat_type_counts
            .iter()
            .map(|(threat, count)| format!("{threat}: {count}"))
            .collect();
        lines.push(format!("  Threat types: {}", parts.join(", ")));
    }

    if !latest_threats.is_empty() {
        lines.push(String::new());
        lines.push(format!(
            "  Latest threat{}:",
            if latest_threats.len() > 1 { "s" } else { "" }
        ));
        for event in &latest_threats {
            let verdict = {
                let verdict = result_str(event, "verdict");
                if verdict == "?" || verdict.is_empty() {
                    "?".to_owned()
                } else {
                    verdict.to_uppercase()
                }
            };
            let threat_type = {
                let threat = result_str(event, "threat_type");
                if threat.is_empty() {
                    "unknown".to_owned()
                } else {
                    threat
                }
            };
            let summary = result_str(event, "summary");
            let ts = format_timestamp(str_of(event, "timestamp"));
            lines.push(format!("    [{ts}] {verdict} — {threat_type}: {summary}"));
        }
    }

    lines.join("\n")
}

fn summarize_pii_scan(events: &[Value]) -> String {
    let mut lines = vec!["--- PII Scan ---".to_owned()];

    let mut ok_count = 0_usize;
    let mut verdict_counts: std::collections::BTreeMap<String, u64> =
        std::collections::BTreeMap::new();
    let mut type_counts: std::collections::BTreeMap<String, u64> =
        std::collections::BTreeMap::new();

    for event in events {
        if str_of(event, "result") == "succeeded" {
            ok_count += 1;
            let verdict = {
                let verdict = result_str(event, "verdict");
                if verdict.is_empty() {
                    "unknown".to_owned()
                } else {
                    verdict
                }
            };
            *verdict_counts.entry(verdict).or_insert(0) += 1;
            if let Some(by_type) = result_of(event).get("summary").and_then(Value::as_object) {
                for (pii_type, count) in by_type {
                    if let Some(count) = count.as_u64() {
                        *type_counts.entry(pii_type.clone()).or_insert(0) += count;
                    }
                }
            }
        }
    }
    let fail_count = events.len() - ok_count;

    lines.push(format!(
        "  Scans performed: {} (succeeded: {ok_count}, failed: {fail_count})",
        events.len()
    ));

    if !verdict_counts.is_empty() {
        let parts: Vec<String> = verdict_counts
            .iter()
            .map(|(verdict, count)| format!("{verdict}: {count}"))
            .collect();
        lines.push(format!("  Verdict breakdown: {}", parts.join(", ")));
    }

    if !type_counts.is_empty() {
        let parts: Vec<String> = type_counts
            .iter()
            .map(|(pii_type, count)| format!("{pii_type}: {count}"))
            .collect();
        lines.push(format!("  Finding types: {}", parts.join(", ")));
    }

    lines.join("\n")
}

fn summarize_skill_ledger(events: &[Value]) -> String {
    let mut lines = vec!["--- Skill Ledger ---".to_owned()];

    let mut checks: Vec<&Value> = Vec::new();
    let mut certifications: Vec<&Value> = Vec::new();
    for event in events {
        let command = result_str(event, "command");
        if command == "check" {
            checks.push(event);
        } else if command == "certify" {
            certifications.push(event);
        }
    }

    let checks_ok = checks
        .iter()
        .filter(|event| str_of(event, "result") == "succeeded")
        .count();
    let checks_fail = checks.len() - checks_ok;
    lines.push(format!(
        "  Checks performed: {} (succeeded: {checks_ok}, failed: {checks_fail})",
        checks.len()
    ));

    if !certifications.is_empty() {
        let cert_ok = certifications
            .iter()
            .filter(|event| str_of(event, "result") == "succeeded")
            .count();
        let mut scan_status_counts: std::collections::BTreeMap<String, u64> =
            std::collections::BTreeMap::new();
        for event in &certifications {
            if str_of(event, "result") == "succeeded" {
                let status = {
                    let verdict = result_str(event, "verdict");
                    if verdict.is_empty() {
                        let scan = result_str(event, "scanStatus");
                        if scan.is_empty() {
                            "unknown".to_owned()
                        } else {
                            scan
                        }
                    } else {
                        verdict
                    }
                };
                *scan_status_counts.entry(status).or_insert(0) += 1;
            }
        }
        let parts: Vec<String> = scan_status_counts
            .iter()
            .map(|(status, count)| format!("{status}: {count}"))
            .collect();
        lines.push(format!(
            "  Certifications:   {cert_ok} ({})",
            parts.join(", ")
        ));
    }

    // Latest status per skill, deduplicated by skill_dir (newest first).
    let mut latest_per_skill: std::collections::BTreeMap<String, &Value> =
        std::collections::BTreeMap::new();
    for event in &checks {
        if str_of(event, "result") != "succeeded" {
            continue;
        }
        let skill_dir = request_str(event, "skill_dir");
        if skill_dir.is_empty() {
            continue;
        }
        latest_per_skill.entry(skill_dir).or_insert(event);
    }

    if !latest_per_skill.is_empty() {
        lines.extend(skill_status_lines(&latest_per_skill));
    }

    lines.join("\n")
}

/// The per-skill status block of the skill-ledger section: tracked count,
/// status breakdown, and the capped tampered/denied alert lines.
fn skill_status_lines(
    latest_per_skill: &std::collections::BTreeMap<String, &Value>,
) -> Vec<String> {
    let mut lines = Vec::new();
    let mut status_counts: std::collections::BTreeMap<String, u64> =
        std::collections::BTreeMap::new();
    let mut tampered: Vec<(String, &Value)> = Vec::new();
    let mut denied: Vec<(String, &Value)> = Vec::new();

    for (skill_dir, event) in latest_per_skill {
        let status = non_empty_or(result_str(event, "status"), "unknown");
        *status_counts.entry(status.clone()).or_insert(0) += 1;
        let skill_name = std::path::Path::new(skill_dir)
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or_default()
            .to_owned();
        if status == "tampered" {
            tampered.push((skill_name, event));
        } else if status == "deny" {
            denied.push((skill_name, event));
        }
    }

    lines.push(String::new());
    lines.push(format!("  Skills tracked: {}", latest_per_skill.len()));
    let parts: Vec<String> = status_counts
        .iter()
        .map(|(status, count)| format!("{status}: {count}"))
        .collect();
    lines.push(format!("  Status: {}", parts.join(", ")));

    if !tampered.is_empty() {
        lines.push(String::new());
        lines.push(format!("  Tampered ({}):", tampered.len()));
        for (skill_name, event) in tampered.iter().take(3) {
            let reason = non_empty_or(result_str(event, "reason"), "signature mismatch");
            let ts = format_timestamp(str_of(event, "timestamp"));
            lines.push(format!("    [{ts}] {skill_name} — {reason}"));
        }
    }

    if !denied.is_empty() {
        lines.push(String::new());
        lines.push(format!("  Denied ({}):", denied.len()));
        for (skill_name, event) in denied.iter().take(3) {
            let ts = format_timestamp(str_of(event, "timestamp"));
            lines.push(format!("    [{ts}] {skill_name} — high-risk findings"));
        }
    }

    lines
}

/// `value` unless it is empty, then `fallback`.
fn non_empty_or(value: String, fallback: &str) -> String {
    if value.is_empty() {
        fallback.to_owned()
    } else {
        value
    }
}

/// Per-skill latest status counts for posture and suggestions.
fn skill_ledger_latest_statuses(events: &[Value]) -> std::collections::BTreeMap<String, u64> {
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut counts: std::collections::BTreeMap<String, u64> = std::collections::BTreeMap::new();
    for event in events {
        if str_of(event, "result") != "succeeded" {
            continue;
        }
        if result_str(event, "command") != "check" {
            continue;
        }
        let skill_dir = request_str(event, "skill_dir");
        if skill_dir.is_empty() || !seen.insert(skill_dir) {
            continue;
        }
        let status = {
            let status = result_str(event, "status");
            if status.is_empty() {
                "unknown".to_owned()
            } else {
                status
            }
        };
        *counts.entry(status).or_insert(0) += 1;
    }
    counts
}

fn compute_posture(
    hardening: &[Value],
    verify: &[Value],
    prompt_scan: &[Value],
    pii_scan: &[Value],
    ledger_statuses: &std::collections::BTreeMap<String, u64>,
    time_label: &str,
) -> String {
    let mut needs_attention = false;

    if let Some(latest_harden) = hardening.first() {
        if str_of(latest_harden, "result") == "failed" {
            needs_attention = true;
        } else if str_of(latest_harden, "result") == "succeeded" {
            let failures_present = result_of(latest_harden)
                .get("failures")
                .is_some_and(|failures| !failures.as_array().unwrap_or(&Vec::new()).is_empty());
            if failures_present {
                needs_attention = true;
            }
        }
    }

    let latest_full_verify = verify
        .iter()
        .find(|event| is_full_verify(event) && asset_verify_outcome(event) != "no_candidates");
    if let Some(latest_full_verify) = latest_full_verify
        && asset_verify_outcome(latest_full_verify) == "failed"
    {
        needs_attention = true;
    }

    for event in prompt_scan {
        if str_of(event, "result") == "succeeded" && result_str(event, "verdict") == "deny" {
            needs_attention = true;
            break;
        }
    }

    for event in pii_scan {
        if str_of(event, "result") == "succeeded" && result_str(event, "verdict") == "deny" {
            needs_attention = true;
            break;
        }
    }

    if ledger_statuses.get("tampered").copied().unwrap_or(0) > 0
        || ledger_statuses.get("deny").copied().unwrap_or(0) > 0
    {
        needs_attention = true;
    }

    let status_line = if needs_attention {
        "System Status: Needs attention \u{26a0}"
    } else {
        "System Status: Good \u{2713}"
    };

    format!("Security Posture Summary ({time_label})\n\n{status_line}")
}

fn build_footer(
    events: &[Value],
    hardening: &[Value],
    ledger_statuses: &std::collections::BTreeMap<String, u64>,
) -> String {
    let total = events.len();
    let failed = events
        .iter()
        .filter(|event| str_of(event, "result") == "failed")
        .count();

    let last_event_str = events
        .iter()
        .map(|event| str_of(event, "timestamp"))
        .max()
        .map_or_else(|| "N/A".to_owned(), time_since_last_event);

    let mut lines = vec![format!(
        "---\nTotal events: {total}  |  Failed: {failed}  |  Last event: {last_event_str}"
    )];

    let suggestions = compute_suggestions(hardening, ledger_statuses);
    if !suggestions.is_empty() {
        lines.push(String::new());
        lines.push("Suggested actions:".to_owned());
        for suggestion in suggestions {
            lines.push(format!("  {suggestion}"));
        }
    }

    lines.join("\n")
}

/// Human-readable time since one ISO-8601 timestamp, v1 wording.
fn time_since_last_event(timestamp: &str) -> String {
    let Ok(moment) = chrono::DateTime::parse_from_rfc3339(timestamp) else {
        return "unknown".to_owned();
    };
    let delta = chrono::Utc::now().signed_duration_since(moment);
    let minutes = delta.num_minutes();
    if minutes < 1 {
        return "just now".to_owned();
    }
    if minutes < 60 {
        return format!("{minutes} min ago");
    }
    let hours = minutes / 60;
    if hours < 24 {
        return format!("{hours}h ago");
    }
    format!("{}d ago", hours / 24)
}

fn compute_suggestions(
    hardening: &[Value],
    ledger_statuses: &std::collections::BTreeMap<String, u64>,
) -> Vec<String> {
    let mut suggestions: Vec<String> = Vec::new();

    if let Some(latest) = hardening.first()
        && has_actionable_hardening_failure(latest)
        && (str_of(latest, "result") == "succeeded" || has_hardening_stats(latest))
    {
        suggestions.push("agent-sec-cli harden --reinforce    Fix failed rules".to_owned());
    }

    if !ledger_statuses.is_empty() {
        const LEDGER_HINTS: [(&str, &str); 3] = [
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
        for (status_key, hint) in LEDGER_HINTS {
            if ledger_statuses.get(status_key).copied().unwrap_or(0) > 0 {
                suggestions.push(hint.to_owned());
            }
        }
    }

    suggestions
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn scan_event(category: &str, result: &str, details: &Value) -> Value {
        json!({
            "event_id": format!("{category}-1"),
            "event_type": "sandbox_prehook",
            "category": category,
            "result": result,
            "timestamp": "2026-01-02T00:00:00+00:00",
            "details": details,
        })
    }

    #[test]
    fn an_empty_window_says_no_events_recorded() {
        assert_eq!(
            format_summary(&[], "last 24 hours"),
            "No security events recorded.\n"
        );
    }

    #[test]
    fn a_deny_verdict_needs_attention() {
        let events = vec![scan_event(
            "prompt_scan",
            "succeeded",
            &json!({"result": {"verdict": "deny", "threat_type": "prompt_injection", "summary": "bad"}}),
        )];
        let text = format_summary(&events, "last 24 hours");
        assert!(
            text.contains("Security Posture Summary (last 24 hours)"),
            "{text}"
        );
        assert!(text.contains("System Status: Needs attention"), "{text}");
        assert!(text.contains("--- Prompt Scan ---"), "{text}");
        assert!(text.contains("Verdict breakdown: deny: 1"), "{text}");
        assert!(text.contains("Threat types: prompt_injection: 1"), "{text}");
        assert!(text.contains("Total events: 1  |  Failed: 0"), "{text}");
    }

    #[test]
    fn a_tampered_skill_needs_attention_and_suggests_a_check() {
        let events = vec![scan_event(
            "skill_ledger",
            "succeeded",
            &json!({
                "result": {"command": "check", "status": "tampered", "reason": "signature mismatch"},
                "request": {"skill_dir": "/opt/skills/demo"}
            }),
        )];
        let text = format_summary(&events, "last 24 hours");
        assert!(text.contains("System Status: Needs attention"), "{text}");
        assert!(text.contains("Tampered (1):"), "{text}");
        assert!(text.contains("demo — signature mismatch"), "{text}");
        assert!(
            text.contains("agent-sec-cli skill-ledger check <dir>"),
            "{text}"
        );
    }

    #[test]
    fn a_benign_window_is_good() {
        let events = vec![scan_event(
            "code_scan",
            "succeeded",
            &json!({"result": {"verdict": "pass"}}),
        )];
        let text = format_summary(&events, "last 24 hours");
        assert!(text.contains("System Status: Good"), "{text}");
        assert!(text.contains("--- Code Scanning ---"), "{text}");
        assert!(text.contains("Verdict: pass: 1"), "{text}");
    }

    #[test]
    fn hardening_compliance_uses_the_latest_scan_statistics() {
        let events = vec![scan_event(
            "hardening",
            "succeeded",
            &json!({"result": {"mode": "scan", "passed": 8, "total": 10, "failures": [
                {"rule_id": "r-1", "status": "FAIL"}
            ]}}),
        )];
        let text = format_summary(&events, "last 2 hours");
        assert!(text.contains("--- Hardening ---"), "{text}");
        assert!(
            text.contains("Scans performed:  1 (succeeded: 1, failed: 0)"),
            "{text}"
        );
        assert!(
            text.contains("Compliance: 8/10 rules passed (80.0%)"),
            "{text}"
        );
        assert!(text.contains("harden --reinforce"), "{text}");
        assert!(text.contains("System Status: Needs attention"), "{text}");
    }

    #[test]
    fn full_verify_outcomes_drive_the_posture() {
        let failed = vec![scan_event(
            "asset_verify",
            "succeeded",
            &json!({"result": {"passed": 1, "failed": 2}, "request": {}}),
        )];
        let text = format_summary(&failed, "last 24 hours");
        assert!(text.contains("--- Asset Verification ---"), "{text}");
        assert!(
            text.contains("Integrity status: FAILURES DETECTED"),
            "{text}"
        );
        assert!(text.contains("System Status: Needs attention"), "{text}");
    }
}
