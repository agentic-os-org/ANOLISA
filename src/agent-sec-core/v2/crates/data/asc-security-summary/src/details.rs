//! Detail-payload accessors shared by every section.
//!
//! Migrated from the `_safe_details` / `_get_result` / `_get_request` /
//! `_get_mode` / `_is_full_verify` / `_asset_verify_outcome` /
//! `_has_hardening_stats` / `_has_actionable_hardening_failure` helpers of v1
//! `security_events/summary_formatter.py`.
//!
//! v1 has to defend against `details` not being a dict at all; in v2 the field is
//! typed as a `Map`, so `_safe_details` collapses into a field access and only the
//! *nested* shape checks remain.

use std::sync::LazyLock;

use asc_security_events::{EventResult, SecurityEvent};
use serde_json::{Map, Value};

static EMPTY: LazyLock<Map<String, Value>> = LazyLock::new(Map::new);

/// Returns `details.result` when it is an object, else an empty map.
pub(crate) fn result_of(event: &SecurityEvent) -> &Map<String, Value> {
    nested(&event.details, "result")
}

/// Returns `details.request` when it is an object, else an empty map.
pub(crate) fn request_of(event: &SecurityEvent) -> &Map<String, Value> {
    nested(&event.details, "request")
}

fn nested<'a>(map: &'a Map<String, Value>, key: &str) -> &'a Map<String, Value> {
    match map.get(key) {
        Some(Value::Object(inner)) => inner,
        _ => &EMPTY,
    }
}

/// Renders `value` the way v1's f-strings do: strings bare, everything else
/// through Python's `str()`.
///
/// The three scalar spellings below are not cosmetic. `details.result.verdict`
/// may legitimately be present and `null`, which v1 uses as its own counter key
/// and prints as `None` — the differential harness caught `null` here. Booleans
/// get the same treatment for the same reason.
///
/// Containers remain a known divergence: Python prints a `dict`'s `repr` with
/// single quotes, and reproducing that faithfully would mean re-implementing
/// Python's repr for arbitrarily nested values. `serde_json`'s compact form is
/// used instead, and it is only reachable for payloads v1 itself never produces
/// (see D-16).
pub(crate) fn text(value: &Value) -> String {
    match value {
        Value::String(inner) => inner.clone(),
        Value::Null => "None".to_owned(),
        Value::Bool(true) => "True".to_owned(),
        Value::Bool(false) => "False".to_owned(),
        other => other.to_string(),
    }
}

/// Returns `map[key]` rendered as a counter key, or `default` when absent.
///
/// v1 uses `result.get("verdict", "unknown")` directly as a dict key, so a
/// present-but-null value counts as its own bucket rather than falling back.
pub(crate) fn counter_key(map: &Map<String, Value>, key: &str, default: &str) -> String {
    map.get(key).map_or_else(|| default.to_owned(), text)
}

/// Returns `map[key]` as an integer, treating anything else as v1's `0` default.
pub(crate) fn int_field(map: &Map<String, Value>, key: &str) -> i64 {
    map.get(key).and_then(Value::as_i64).unwrap_or(0)
}

/// Returns whether `map[key]` is the number zero, the way Python's `== 0` reads.
pub(crate) fn is_zero(map: &Map<String, Value>, key: &str) -> bool {
    match map.get(key) {
        None => true,
        Some(Value::Number(number)) => number.as_f64().is_some_and(|value| value == 0.0),
        Some(Value::Bool(flag)) => !flag,
        Some(_) => false,
    }
}

/// Returns the hardening mode, falling back to parsing `request.args`.
///
/// v1 reads `result.mode` and returns it unchanged when truthy — a non-string
/// value therefore matches neither `scan` nor `reinforce` and silently excludes
/// the event from both buckets. Rendering it through [`text`] preserves that.
pub(crate) fn mode_of(event: &SecurityEvent) -> String {
    let result = result_of(event);
    if let Some(mode) = result.get("mode")
        && is_truthy(mode)
    {
        return text(mode);
    }
    let Some(Value::Array(args)) = request_of(event).get("args") else {
        return String::new();
    };
    for (flag, mode) in [
        ("--dry-run", "dry-run"),
        ("--reinforce", "reinforce"),
        ("--scan", "scan"),
    ] {
        if args.iter().any(|arg| arg == flag) {
            return mode.to_owned();
        }
    }
    String::new()
}

/// Python truthiness, needed because v1 gates on `if mode:` and `if failures:`.
pub(crate) fn is_truthy(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(flag) => *flag,
        Value::Number(number) => number.as_f64().is_some_and(|inner| inner != 0.0),
        Value::String(inner) => !inner.is_empty(),
        Value::Array(items) => !items.is_empty(),
        Value::Object(map) => !map.is_empty(),
    }
}

/// Returns whether a hardening event carries a parsed rule summary.
pub(crate) fn has_hardening_stats(event: &SecurityEvent) -> bool {
    result_of(event)
        .get("total")
        .and_then(Value::as_i64)
        .is_some_and(|total| total > 0)
}

/// Returns whether a hardening event names at least one fixable failed rule.
pub(crate) fn has_actionable_hardening_failure(event: &SecurityEvent) -> bool {
    let Some(Value::Array(failures)) = result_of(event).get("failures") else {
        return false;
    };
    failures.iter().any(|failure| {
        let Value::Object(failure) = failure else {
            return false;
        };
        if failure.get("status").and_then(Value::as_str) == Some("UNKNOWN") {
            return false;
        }
        failure.get("rule_id").is_some_and(is_truthy)
    })
}

/// Returns whether the event is a whole-tree verification.
///
/// v1: full verify runs carry `request.skill == None`; a single-skill run names
/// the skill.
pub(crate) fn is_full_verify(event: &SecurityEvent) -> bool {
    matches!(request_of(event).get("skill"), None | Some(Value::Null))
}

/// The three semantic outcomes an `asset_verify` event can carry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum VerifyOutcome {
    /// Every candidate passed.
    Verified,
    /// At least one candidate failed.
    Failed,
    /// Nothing was assessed.
    NoCandidates,
}

/// Returns the verification outcome, reconstructing it for legacy events that
/// predate the explicit `outcome` field.
pub(crate) fn verify_outcome(event: &SecurityEvent) -> VerifyOutcome {
    let result = result_of(event);
    match result.get("outcome").and_then(Value::as_str) {
        Some("verified") => return VerifyOutcome::Verified,
        Some("failed") => return VerifyOutcome::Failed,
        Some("no_candidates") => return VerifyOutcome::NoCandidates,
        _ => {}
    }

    if event.result == EventResult::Failed {
        return VerifyOutcome::Failed;
    }
    if int_field(result, "failed") > 0 {
        return VerifyOutcome::Failed;
    }
    if is_zero(result, "passed") && is_zero(result, "failed") {
        return VerifyOutcome::NoCandidates;
    }
    VerifyOutcome::Verified
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn event_with(details: Value) -> SecurityEvent {
        let Value::Object(details) = details else {
            unreachable!("test fixtures always pass an object")
        };
        SecurityEvent::new("t", "c", details)
    }

    #[test]
    fn a_non_object_nested_field_reads_as_empty() {
        let event = event_with(json!({"result": 5, "request": "x"}));
        assert!(result_of(&event).is_empty());
        assert!(request_of(&event).is_empty());
    }

    #[test]
    fn the_mode_comes_from_the_result_when_present() {
        let event = event_with(json!({"result": {"mode": "reinforce"}}));
        assert_eq!(mode_of(&event), "reinforce");
    }

    #[test]
    fn the_mode_falls_back_to_the_request_args_in_flag_priority_order() {
        let event = event_with(json!({"request": {"args": ["--scan", "--reinforce"]}}));
        assert_eq!(
            mode_of(&event),
            "reinforce",
            "v1 checks --dry-run, then --reinforce, then --scan"
        );
        let event = event_with(json!({"request": {"args": ["--scan"]}}));
        assert_eq!(mode_of(&event), "scan");
        let event = event_with(json!({"request": {"args": ["--other"]}}));
        assert_eq!(mode_of(&event), "");
        let event = event_with(json!({"request": {"args": "not-a-list"}}));
        assert_eq!(mode_of(&event), "");
    }

    #[test]
    fn an_empty_mode_string_is_falsy_and_falls_through() {
        let event = event_with(json!({
            "result": {"mode": ""},
            "request": {"args": ["--scan"]},
        }));
        assert_eq!(mode_of(&event), "scan");
    }

    #[test]
    fn hardening_stats_need_a_positive_integer_total() {
        assert!(has_hardening_stats(&event_with(
            json!({"result": {"total": 1}})
        )));
        assert!(!has_hardening_stats(&event_with(
            json!({"result": {"total": 0}})
        )));
        assert!(!has_hardening_stats(&event_with(
            json!({"result": {"total": "3"}})
        )));
        assert!(!has_hardening_stats(&event_with(json!({"result": {}}))));
    }

    #[test]
    fn an_actionable_failure_needs_a_rule_id_and_a_known_status() {
        let actionable = event_with(json!({
            "result": {"failures": [{"rule_id": "R-1", "status": "FAIL"}]},
        }));
        assert!(has_actionable_hardening_failure(&actionable));

        let unknown_status = event_with(json!({
            "result": {"failures": [{"rule_id": "R-1", "status": "UNKNOWN"}]},
        }));
        assert!(!has_actionable_hardening_failure(&unknown_status));

        let no_rule_id = event_with(json!({"result": {"failures": [{"status": "FAIL"}]}}));
        assert!(!has_actionable_hardening_failure(&no_rule_id));

        let not_a_list = event_with(json!({"result": {"failures": "R-1"}}));
        assert!(!has_actionable_hardening_failure(&not_a_list));

        let not_a_dict = event_with(json!({"result": {"failures": ["R-1"]}}));
        assert!(!has_actionable_hardening_failure(&not_a_dict));
    }

    #[test]
    fn a_full_verify_is_one_without_a_named_skill() {
        assert!(is_full_verify(&event_with(json!({"request": {}}))));
        assert!(is_full_verify(&event_with(
            json!({"request": {"skill": null}})
        )));
        assert!(!is_full_verify(&event_with(
            json!({"request": {"skill": "a"}})
        )));
    }

    #[test]
    fn an_explicit_outcome_wins_over_the_legacy_reconstruction() {
        let event = event_with(json!({"result": {"outcome": "verified", "failed": 3}}));
        assert_eq!(verify_outcome(&event), VerifyOutcome::Verified);
    }

    #[test]
    fn a_legacy_outcome_is_reconstructed_from_the_counters() {
        let failed = event_with(json!({"result": {"failed": 1}}));
        assert_eq!(verify_outcome(&failed), VerifyOutcome::Failed);

        let empty = event_with(json!({"result": {}}));
        assert_eq!(verify_outcome(&empty), VerifyOutcome::NoCandidates);

        let verified = event_with(json!({"result": {"passed": 2, "failed": 0}}));
        assert_eq!(verify_outcome(&verified), VerifyOutcome::Verified);
    }

    #[test]
    fn a_failed_event_verifies_as_failed_regardless_of_counters() {
        let mut event = event_with(json!({"result": {"passed": 2}}));
        event.result = EventResult::Failed;
        assert_eq!(verify_outcome(&event), VerifyOutcome::Failed);
    }

    #[test]
    fn a_present_but_null_counter_key_is_its_own_bucket() {
        let Value::Object(map) = json!({"verdict": null}) else {
            unreachable!("the fixture is an object")
        };
        // `None`, not `null`: v1 prints the Python value, and the differential
        // harness compares the rendered line byte for byte.
        assert_eq!(counter_key(&map, "verdict", "unknown"), "None");
        assert_eq!(counter_key(&Map::new(), "verdict", "unknown"), "unknown");
    }

    #[test]
    fn scalars_render_with_python_spelling() {
        assert_eq!(text(&Value::Null), "None");
        assert_eq!(text(&json!(true)), "True");
        assert_eq!(text(&json!(false)), "False");
        assert_eq!(text(&json!(3)), "3");
        assert_eq!(text(&json!("deny")), "deny");
    }
}
