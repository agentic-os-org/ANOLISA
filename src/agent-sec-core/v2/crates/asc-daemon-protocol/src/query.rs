//! Read-only query request parameters.
//!
//! These are the untrusted wire values of the `sec.*` query family. The v1
//! daemon served the same methods from one flat parameter dictionary, so the
//! field names here are v1's wire names (`snake_case`) rather than the
//! `camelCase` this protocol uses for its own newer families — a v1 dashboard
//! or script must keep working against the v2 daemon unchanged.
//!
//! Owner identity is deliberately absent: the scope comes from the
//! kernel-authenticated peer, never from these values.

use serde::{Deserialize, Serialize};

/// Parameters of the `sec.*` query family.
///
/// One struct serves all four methods, exactly as v1's handlers all read from
/// the same parameter dictionary: `sec.events.get` requires `event_id`,
/// `sec.events.count_by` requires `group_by` and rejects `limit`/`offset`, and
/// the handler enforces those per-method rules after decoding.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct SecQueryParams {
    /// Exact `event_id`; required by `sec.events.get`.
    pub event_id: Option<String>,
    /// Exact `event_type` filter.
    pub event_type: Option<String>,
    /// Exact `category` filter.
    pub category: Option<String>,
    /// Exact `result` filter; one of `failed` / `succeeded`.
    pub result: Option<String>,
    /// Exact `trace_id` filter.
    pub trace_id: Option<String>,
    /// Exact `session_id` filter.
    pub session_id: Option<String>,
    /// Exact `run_id` filter.
    pub run_id: Option<String>,
    /// Exact `call_id` filter.
    pub call_id: Option<String>,
    /// Exact `tool_call_id` filter.
    pub tool_call_id: Option<String>,
    /// Exact `verdict` filter.
    pub verdict: Option<String>,
    /// Inclusive lower bound, ISO-8601; mutually exclusive with `start_ns`.
    pub since: Option<String>,
    /// Exclusive upper bound, ISO-8601; mutually exclusive with `end_ns`.
    pub until: Option<String>,
    /// Inclusive lower bound as epoch nanoseconds.
    pub start_ns: Option<u64>,
    /// Exclusive upper bound as epoch nanoseconds.
    pub end_ns: Option<u64>,
    /// Page size of `sec.events.list`; defaults to 100, at most 1000.
    pub limit: Option<u64>,
    /// Page offset of `sec.events.list`; defaults to 0.
    pub offset: Option<u64>,
    /// Whether `sec.events.list` rows carry their `details` payload.
    pub include_details: Option<bool>,
    /// Row count of the summary's `latest_events`; defaults to 5, at most 50.
    pub latest_limit: Option<u64>,
    /// Group field of `sec.events.count_by`; required by that method.
    pub group_by: Option<String>,
    /// Owner filter within the authorized scope.
    ///
    /// Root may select any UID (or omit it to read all owners); a non-root
    /// caller may only select its own UID. This filters inside the
    /// kernel-derived scope — it never widens it.
    pub owner_uid: Option<u32>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_empty_object_decodes_with_all_defaults() {
        let params: SecQueryParams =
            serde_json::from_value(serde_json::json!({})).expect("empty params decode");
        assert_eq!(params, SecQueryParams::default());
    }

    #[test]
    fn v1_field_names_decode_verbatim() {
        let params: SecQueryParams = serde_json::from_value(serde_json::json!({
            "event_type": "sandbox_prehook",
            "category": "exec",
            "result": "failed",
            "trace_id": "t-1",
            "session_id": "s-1",
            "run_id": "r-1",
            "call_id": "c-1",
            "tool_call_id": "tc-1",
            "verdict": "deny",
            "since": "2026-01-01T00:00:00+00:00",
            "until": "2026-01-02T00:00:00+00:00",
            "limit": 20,
            "offset": 40,
            "include_details": true,
            "event_id": "e-1",
            "group_by": "category",
            "latest_limit": 10,
            "start_ns": 1,
            "end_ns": 2,
        }))
        .expect("v1 wire names decode");
        assert_eq!(params.event_type.as_deref(), Some("sandbox_prehook"));
        assert_eq!(params.limit, Some(20));
        assert_eq!(params.include_details, Some(true));
        assert_eq!(params.group_by.as_deref(), Some("category"));
    }

    #[test]
    fn unknown_fields_are_rejected() {
        let decoded = serde_json::from_value::<SecQueryParams>(serde_json::json!({
            "event_type": "x",
            "ownerUid": 0,
        }));
        assert!(
            decoded.is_err(),
            "a caller must not be able to name a scope"
        );
    }

    #[test]
    fn non_integer_pagination_is_rejected_at_decode() {
        let decoded = serde_json::from_value::<SecQueryParams>(serde_json::json!({
            "limit": true,
        }));
        assert!(decoded.is_err(), "booleans are not integers");
        let decoded = serde_json::from_value::<SecQueryParams>(serde_json::json!({
            "limit": -5,
        }));
        assert!(decoded.is_err(), "negative integers are not unsigned");
    }
}
