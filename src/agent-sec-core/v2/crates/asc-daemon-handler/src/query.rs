//! Query configuration and owner-scoped security-event projection over the read store.
//!
//! This is the v2 restoration of v1's `sec.*` dashboard query family
//! (`agent_sec_cli/daemon/handlers/security_query.py`). Two things changed
//! with the system daemon, and both are security-relevant:
//!
//! * **The store is shared.** v1 isolated owners by running one daemon per
//!   user over a 0600 socket and a per-user database; v2 serves many local
//!   UIDs from one system store, so every read here carries a
//!   [`QueryScope`] derived from the kernel-authenticated peer. A caller
//!   cannot name, widen, or hint at a scope through request parameters.
//! * **Cross-owner audit is absent on purpose.** Outside the kernel-derived
//!   scope the server assigns no auditor role, so no non-root principal —
//!   administrator or not — reads another owner's rows through these
//!   methods (issue #6608). The one widening is the root peer, whose scope
//!   is all owners by design.
//!
//! The query port and its scope live in `asc-security-events`; the `SQLite`
//! adapter lives in `asc-persistence-sqlite` and is bound by the daemon
//! composition root. That layering keeps this crate storage-free, which the
//! architecture gate in `tests/v2/test_action_architecture.py` enforces.

use std::path::Path;

use asc_daemon_core::Principal;
use asc_daemon_protocol::{
    DaemonResponse, MAX_DAEMON_ERROR_MESSAGE_BYTES, RequestId, SecQueryParams, error_code, method,
};
use asc_security_events::query::{
    EventFilters, GroupCounts, QueryError, QueryScope, SecurityEventQueries, VALID_GROUP_FIELDS,
};
use asc_security_events::timestamp::{NaivePolicy, normalize_iso_to_utc_iso, utc_iso_to_epoch};
use asc_security_events::{SecurityEvent, extract_verdict};
use serde_json::{Map, Value, json};

/// v1's default page size for `sec.events.list`.
const DEFAULT_LIMIT: u64 = 100;
/// v1's hard cap on one page of `sec.events.list`.
const MAX_LIMIT: u64 = 1000;
/// v1's default row count for the summary's `latest_events`.
const DEFAULT_LATEST_LIMIT: u64 = 5;
/// v1's hard cap on the summary's `latest_events`.
const MAX_LATEST_LIMIT: u64 = 50;
/// v1's accepted `result` values.
const EVENT_RESULTS: [&str; 2] = ["failed", "succeeded"];

/// Protocol adapters for security-event and observability queries.
#[derive(Default)]
pub struct QueryHandler {
    source: Option<Box<dyn SecurityEventQueries>>,
    pub(super) observability: Option<asc_daemon_core::query::ObservabilityQueryService>,
}

impl QueryHandler {
    /// Binds the `sec.*` query family to its configured store.
    #[must_use]
    pub fn with_security_queries(mut self, source: impl SecurityEventQueries + 'static) -> Self {
        self.source = Some(Box::new(source));
        self
    }

    /// Binds the `obs.*` query family independently of security-event reads.
    #[must_use]
    pub fn with_observability_queries(
        mut self,
        service: asc_daemon_core::query::ObservabilityQueryService,
    ) -> Self {
        self.observability = Some(service);
        self
    }

    /// Serves one query method for one authenticated principal.
    pub fn handle(
        &self,
        request_id: RequestId,
        principal: &Principal,
        query: method::QueryMethod,
        params: Value,
    ) -> DaemonResponse {
        let Some(source) = self.source.as_deref() else {
            return DaemonResponse::error(
                request_id,
                error_code::UNAVAILABLE,
                "security event queries are not configured",
            );
        };
        let params: SecQueryParams = match serde_json::from_value(params) {
            Ok(params) => params,
            Err(error) => {
                return DaemonResponse::error(
                    request_id,
                    error_code::INVALID_REQUEST,
                    &bounded_message(&error.to_string()),
                );
            }
        };
        let scope = resolve_scope(principal);
        match query {
            method::QueryMethod::Summary => Self::summary(source, request_id, &params, scope),
            method::QueryMethod::EventsList => Self::list(source, request_id, &params, scope),
            method::QueryMethod::EventsGet => Self::get(source, request_id, &params, scope),
            method::QueryMethod::EventsCountBy => {
                Self::count_by(source, request_id, &params, scope)
            }
        }
    }

    fn summary(
        source: &dyn SecurityEventQueries,
        request_id: RequestId,
        params: &SecQueryParams,
        scope: QueryScope,
    ) -> DaemonResponse {
        let Ok((filters, latest_limit)) = summary_filters(params) else {
            return invalid_parameters(request_id);
        };
        let summary = match source.summary(&filters, scope, latest_limit) {
            Ok(summary) => summary,
            Err(error) => return storage_failure(request_id, &error),
        };
        DaemonResponse::success(
            request_id,
            json!({
                "total": summary.total,
                "by_category": count_map(&summary.by_category),
                "by_event_type": count_map(&summary.by_event_type),
                "by_result": count_map(&summary.by_result),
                "affected_sessions": non_empty_group_count(&summary.by_session),
                "affected_runs": non_empty_group_count(&summary.by_run),
                "latest_events": summary
                    .latest_events
                    .iter()
                    .map(|event| event_payload(event, false))
                    .collect::<Vec<_>>(),
            }),
        )
    }

    fn list(
        source: &dyn SecurityEventQueries,
        request_id: RequestId,
        params: &SecQueryParams,
        scope: QueryScope,
    ) -> DaemonResponse {
        let Ok((filters, limit, offset, include_details)) = list_filters(params) else {
            return invalid_parameters(request_id);
        };
        let items = match source.list(&filters, scope, limit, offset) {
            Ok(items) => items,
            Err(error) => return storage_failure(request_id, &error),
        };
        // `total` is the pre-pagination row count of the whole scope: the
        // client derives its own "rows after offset" view from it, so the
        // cursor math must not fold the offset into the total.
        let total = match source.count(&filters, scope, 0) {
            Ok(total) => total,
            Err(error) => return storage_failure(request_id, &error),
        };
        let next_offset = next_offset(offset, u64::from(limit), items.len() as u64, total);
        DaemonResponse::success(
            request_id,
            json!({
                "items": items
                    .iter()
                    .map(|event| event_payload(event, include_details))
                    .collect::<Vec<_>>(),
                "total": total,
                "limit": limit,
                "offset": offset,
                "next_offset": next_offset,
            }),
        )
    }

    fn get(
        source: &dyn SecurityEventQueries,
        request_id: RequestId,
        params: &SecQueryParams,
        scope: QueryScope,
    ) -> DaemonResponse {
        let Some(event_id) = non_empty(params.event_id.as_deref()) else {
            return invalid_parameters(request_id);
        };
        // A foreign event and a missing event are indistinguishable here, so
        // an event_id guess cannot probe another owner's store.
        let event = match source.get(event_id, scope) {
            Ok(event) => event,
            Err(error) => return storage_failure(request_id, &error),
        };
        DaemonResponse::success(
            request_id,
            json!({
                "found": event.is_some(),
                "event": event.as_ref().map(|event| event_payload(event, true)),
            }),
        )
    }

    fn count_by(
        source: &dyn SecurityEventQueries,
        request_id: RequestId,
        params: &SecQueryParams,
        scope: QueryScope,
    ) -> DaemonResponse {
        if params.limit.is_some() {
            return invalid_parameters(request_id);
        }
        let Some(group_by) = non_empty(params.group_by.as_deref()) else {
            return invalid_parameters(request_id);
        };
        if !VALID_GROUP_FIELDS.contains(&group_by) {
            return invalid_parameters(request_id);
        }
        let Ok(offset) = i64::try_from(params.offset.unwrap_or(0)) else {
            return invalid_parameters(request_id);
        };
        let Ok(filters) = event_filters(params) else {
            return invalid_parameters(request_id);
        };
        match source.count_by(group_by, &filters, scope, offset) {
            Ok(groups) => DaemonResponse::success(
                request_id,
                json!({
                    "group_by": group_by,
                    "items": count_items(&groups),
                }),
            ),
            Err(QueryError::InvalidGroupField(_)) => invalid_parameters(request_id),
            Err(error) => storage_failure(request_id, &error),
        }
    }
}

/// Derives the read scope from kernel-authenticated evidence.
///
/// Root reads all UIDs. Every non-root peer, including a
/// `PolicyAdministrator`, reads only its own UID.
fn resolve_scope(principal: &Principal) -> QueryScope {
    match principal.peer().uid() {
        0 => QueryScope::All,
        uid => QueryScope::Owner(uid),
    }
}

/// Parses the `sec.summary` filter set.
fn summary_filters(params: &SecQueryParams) -> Result<(EventFilters, u32), ()> {
    if params.limit.is_some()
        || params.offset.is_some()
        || params.include_details.is_some()
        || params.group_by.is_some()
        || params.event_id.is_some()
    {
        return Err(());
    }
    let filters = event_filters(params)?;
    let latest_limit = bounded(
        params.latest_limit,
        DEFAULT_LATEST_LIMIT,
        1,
        MAX_LATEST_LIMIT,
    )?;
    Ok((filters, u32::try_from(latest_limit).expect("bounded to 50")))
}

/// Parses the `sec.events.list` filter set.
fn list_filters(params: &SecQueryParams) -> Result<(EventFilters, u32, i64, bool), ()> {
    if params.group_by.is_some() || params.event_id.is_some() || params.latest_limit.is_some() {
        return Err(());
    }
    let filters = event_filters(params)?;
    let limit = bounded(params.limit, DEFAULT_LIMIT, 1, MAX_LIMIT)?;
    // v1's offset is a signed 64-bit quantity clamped at zero; capping at
    // `u32::MAX` narrowed it, so the bound now covers the full non-negative
    // i64 range SQLite's `OFFSET` accepts.
    let offset = bounded(params.offset, 0, 0, i64::MAX as u64)?;
    let include_details = params.include_details.unwrap_or(false);
    Ok((
        filters,
        u32::try_from(limit).expect("bounded to 1000"),
        i64::try_from(offset).expect("bounded to i64::MAX"),
        include_details,
    ))
}

/// Builds the repository filter set from the shared v1 parameter names.
fn event_filters(params: &SecQueryParams) -> Result<EventFilters, ()> {
    if non_empty(params.result.as_deref()).is_some_and(|result| !EVENT_RESULTS.contains(&result)) {
        return Err(());
    }
    let (since_epoch, until_epoch) = time_bounds(params)?;
    Ok(EventFilters {
        event_type: non_empty(params.event_type.as_deref()).map(str::to_owned),
        category: non_empty(params.category.as_deref()).map(str::to_owned),
        result: non_empty(params.result.as_deref()).map(str::to_owned),
        trace_id: non_empty(params.trace_id.as_deref()).map(str::to_owned),
        session_id: non_empty(params.session_id.as_deref()).map(str::to_owned),
        run_id: non_empty(params.run_id.as_deref()).map(str::to_owned),
        call_id: non_empty(params.call_id.as_deref()).map(str::to_owned),
        tool_call_id: non_empty(params.tool_call_id.as_deref()).map(str::to_owned),
        verdict: non_empty(params.verdict.as_deref()).map(str::to_owned),
        since_epoch,
        until_epoch,
    })
}

/// Resolves the v1 time-range parameters to epochs.
///
/// `since`/`until` accept v1's ISO-8601 spellings, including naive local
/// timestamps, which are normalized to UTC at this boundary exactly as v1's
/// handlers did. `start_ns`/`end_ns` are absolute epoch nanoseconds and are
/// mutually exclusive with their ISO counterparts.
fn time_bounds(params: &SecQueryParams) -> Result<(Option<f64>, Option<f64>), ()> {
    if params.since.is_some() && params.start_ns.is_some() {
        return Err(());
    }
    if params.until.is_some() && params.end_ns.is_some() {
        return Err(());
    }
    let since = match (&params.since, params.start_ns) {
        (Some(raw), _) => Some(iso_to_epoch(raw, "since")?),
        (None, Some(nanos)) => Some(epoch_of_nanos(nanos)),
        (None, None) => None,
    };
    let until = match (&params.until, params.end_ns) {
        (Some(raw), _) => Some(iso_to_epoch(raw, "until")?),
        (None, Some(nanos)) => Some(epoch_of_nanos(nanos)),
        (None, None) => None,
    };
    if let (Some(since), Some(until)) = (since, until)
        && since > until
    {
        return Err(());
    }
    Ok((since, until))
}

/// Converts epoch nanoseconds to epoch seconds.
///
/// The split keeps both casts exact for every `u64` input: the seconds part
/// is at most `u64::MAX / 1e9 < 2^34` and the remainder is below `1e9`, both
/// far inside `f64`'s 53-bit mantissa.
#[allow(clippy::cast_precision_loss)]
fn epoch_of_nanos(nanos: u64) -> f64 {
    let seconds = nanos / 1_000_000_000;
    let remainder = nanos % 1_000_000_000;
    seconds as f64 + remainder as f64 / 1e9
}

/// Normalizes one ISO-8601 bound to an epoch, v1 semantics.
///
/// Both failure shapes — an unparsable value and a missing timezone — are the
/// caller's fault and land as the same `invalid_argument` rejection.
fn iso_to_epoch(raw: &str, field: &str) -> Result<f64, ()> {
    let normalized = normalize_iso_to_utc_iso(raw, field, NaivePolicy::Local).map_err(|_| ())?;
    utc_iso_to_epoch(&normalized, field).map_err(|_| ())
}

/// Validates one optional positive-integer parameter against v1's bounds.
fn bounded(value: Option<u64>, default: u64, minimum: u64, maximum: u64) -> Result<u64, ()> {
    let value = value.unwrap_or(default);
    if value < minimum || value > maximum {
        return Err(());
    }
    Ok(value)
}

/// Trims one optional string the way v1's `_optional_string_param` did.
fn non_empty(value: Option<&str>) -> Option<&str> {
    value.map(str::trim).filter(|value| !value.is_empty())
}

/// Computes v1's `next_offset` pagination cursor.
fn next_offset(offset: i64, limit: u64, returned: u64, total: u64) -> Option<u64> {
    let offset = u64::try_from(offset).expect("non-negative");
    if offset + returned < total {
        Some(offset + limit)
    } else {
        None
    }
}

/// Renders one event as the v1 dashboard payload.
///
/// The base object is the event's own serialization, which reproduces v1
/// `to_dict()` field for field; the dashboard adds the derived `verdict` and
/// the `skill_ledger` projection, and drops `details` unless asked to keep it.
fn event_payload(event: &SecurityEvent, include_details: bool) -> Value {
    let mut payload = serde_json::to_value(event).expect("the event serializes");
    let Some(object) = payload.as_object_mut() else {
        return payload;
    };
    if let Some(verdict) = extract_verdict(&event.details) {
        object.insert("verdict".to_owned(), Value::String(verdict));
    }
    if event.category == "skill_ledger" {
        add_skill_ledger_fields(object, &event.details);
    }
    if !include_details {
        object.remove("details");
    }
    payload
}

/// Adds the command and skill-name projection v1's dashboard rendered for
/// skill-ledger events.
fn add_skill_ledger_fields(payload: &mut Map<String, Value>, details: &Map<String, Value>) {
    let result = details.get("result").and_then(Value::as_object);
    let request = details.get("request").and_then(Value::as_object);
    let command = result
        .and_then(|result| first_non_empty_string(result.get("command")))
        .or_else(|| request.and_then(|request| first_non_empty_string(request.get("command"))));
    if let Some(command) = command {
        payload.insert("command".to_owned(), Value::String(command));
    }
    let mut skill_name = result.and_then(|result| first_non_empty_string(result.get("skill_name")));
    if skill_name.is_none() {
        skill_name = request
            .and_then(|request| first_non_empty_string(request.get("skill_dir")))
            .and_then(|skill_dir| {
                Path::new(&skill_dir)
                    .file_name()
                    .and_then(|name| name.to_str())
                    .map(str::to_owned)
                    .filter(|name| !name.is_empty())
            });
    }
    if let Some(skill_name) = skill_name {
        payload.insert("skill_name".to_owned(), Value::String(skill_name));
    }
}

fn first_non_empty_string(value: Option<&Value>) -> Option<String> {
    value
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
}

/// Renders v1's string-keyed count map, dropping empty buckets.
fn count_map(groups: &GroupCounts) -> Map<String, Value> {
    let mut map = Map::new();
    for (key, count) in groups {
        if let Some(key) = key.as_deref().filter(|key| !key.is_empty()) {
            map.insert(key.to_owned(), json!(count));
        }
    }
    map
}

/// Counts the non-empty buckets of one group list.
fn non_empty_group_count(groups: &GroupCounts) -> usize {
    groups
        .iter()
        .filter(|(key, _)| key.as_deref().is_some_and(|key| !key.is_empty()))
        .count()
}

/// Renders v1's sorted `count_by` item list.
///
/// The SQL `NULL` bucket is delivered as `value: null` — v1's CLI rendered
/// it as the `"null"` object key, and the v2 CLI reproduces that spelling.
/// Empty-string groups stay excluded, as in v1's daemon rendering.
fn count_items(groups: &GroupCounts) -> Vec<Value> {
    let mut items: Vec<Value> = groups
        .iter()
        .filter_map(|(key, count)| match key.as_deref() {
            Some(key) if !key.is_empty() => Some(json!({"value": key, "count": count})),
            None => Some(json!({"value": null, "count": count})),
            Some(_) => None,
        })
        .collect();
    items.sort_by(|left, right| {
        let count = right["count"].as_u64().cmp(&left["count"].as_u64());
        count.then_with(|| {
            left["value"]
                .as_str()
                .unwrap_or_default()
                .cmp(right["value"].as_str().unwrap_or_default())
        })
    });
    items
}

fn invalid_parameters(request_id: RequestId) -> DaemonResponse {
    DaemonResponse::error(
        request_id,
        error_code::INVALID_ARGUMENT,
        "query parameters are invalid",
    )
}

/// The store is present but cannot be read; this must not look like "no
/// events".
fn storage_failure(request_id: RequestId, error: &QueryError) -> DaemonResponse {
    DaemonResponse::error(
        request_id,
        error_code::UNAVAILABLE,
        &bounded_message(&format!(
            "security event store is unavailable or corrupt: {error}"
        )),
    )
}

fn bounded_message(message: &str) -> String {
    if message.len() > MAX_DAEMON_ERROR_MESSAGE_BYTES {
        "request parameters are invalid".to_owned()
    } else {
        message.to_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use asc_daemon_core::{PeerCredentials, PrincipalRole};
    use asc_persistence_sqlite::security_events::SqliteEventQuerySource;
    use asc_persistence_sqlite::security_events::writer::SqliteEventWriter;
    use serde_json::json;
    use tempfile::TempDir;

    fn seeded_source(
        rows: &[(&str, u32, &str, Option<&str>)],
    ) -> (TempDir, SqliteEventQuerySource) {
        let dir = TempDir::new().expect("temp dir");
        let path = dir.path().join("events.db");
        let writer = SqliteEventWriter::new(&path).expect("writer");
        for &(id, uid, category, verdict) in rows {
            let mut event = SecurityEvent::new("sandbox_prehook", category, Map::new());
            id.clone_into(&mut event.event_id);
            event.uid = uid;
            event.session_id = Some("s-1".to_owned());
            if let Some(verdict) = verdict {
                event.details.insert("verdict".to_owned(), json!(verdict));
            }
            writer.write(&event);
        }
        writer.close_at(1000.0);
        let source = SqliteEventQuerySource::new(&path).expect("source");
        (dir, source)
    }

    fn two_owner_source() -> (TempDir, SqliteEventQuerySource) {
        seeded_source(&[
            ("a1", 1000_u32, "exec", Some("deny")),
            ("a2", 1000, "network", None),
            ("b1", 2000, "exec", Some("allow")),
        ])
    }

    fn principal(uid: u32) -> Principal {
        Principal::from_authenticated_peer(
            PeerCredentials::new(uid, 100, 7),
            PrincipalRole::LocalUser,
        )
    }

    fn request_id() -> RequestId {
        RequestId::new("test".to_owned()).expect("non-empty")
    }

    fn handle(
        handler: &QueryHandler,
        uid: u32,
        method_name: &str,
        params: Value,
    ) -> DaemonResponse {
        let query = method::resolve(method_name).expect("registered method");
        let method::MethodId::Query(query) = query else {
            panic!("not a query method");
        };
        handler.handle(request_id(), &principal(uid), query, params)
    }

    fn success_data(response: DaemonResponse) -> Value {
        match response {
            DaemonResponse::Success(success) => success.result,
            DaemonResponse::Error(error) => panic!("expected success, got {error:?}"),
        }
    }

    fn error_code_of(response: DaemonResponse) -> String {
        match response {
            DaemonResponse::Success(_) => panic!("expected error"),
            DaemonResponse::Error(error) => error.error.code.as_str().to_owned(),
        }
    }

    #[test]
    fn query_families_are_available_only_when_independently_configured() {
        use asc_daemon_core::query::ObservabilityQueryService;
        use asc_persistence_sqlite::observability::owned::OwnedObservabilityWriter;
        use asc_persistence_sqlite::query::{SqliteObservabilityQueries, SqliteSecurityQueries};

        for (security, observations) in [(false, false), (true, false), (false, true), (true, true)]
        {
            let (dir, source) = two_owner_source();
            let mut handler = QueryHandler::default();
            if observations {
                let path = dir.path().join("observability.db");
                OwnedObservabilityWriter::new(&path)
                    .unwrap()
                    .probe()
                    .unwrap();
                handler = handler.with_observability_queries(ObservabilityQueryService::new(
                    SqliteObservabilityQueries::new(path),
                    SqliteSecurityQueries::new(dir.path().join("events.db")),
                ));
            }
            if security {
                handler = handler.with_security_queries(source);
            }
            let control = asc_daemon_service::DispatchControl::new(
                std::time::Instant::now() + std::time::Duration::from_secs(5),
            );
            for (response, configured) in [
                (
                    handle(&handler, 1000, method::SEC_SUMMARY, json!({})),
                    security,
                ),
                (
                    handler.handle_observability(
                        request_id(),
                        PeerCredentials::new(1000, 100, 7),
                        &control,
                        method::ObservabilityQueryMethod::Sessions,
                        json!({}),
                    ),
                    observations,
                ),
            ] {
                if configured {
                    success_data(response);
                } else {
                    assert_eq!(error_code_of(response), "unavailable");
                }
            }
        }
    }

    #[test]
    fn one_owner_never_sees_another_owners_rows() {
        let (_dir, source) = two_owner_source();
        let handler = QueryHandler::default().with_security_queries(source);

        let data = success_data(handle(
            &handler,
            1000,
            method::SEC_EVENTS_LIST,
            json!({"session_id":"s-1"}),
        ));
        let ids: Vec<&str> = data["items"]
            .as_array()
            .expect("items array")
            .iter()
            .map(|item| item["event_id"].as_str().expect("id"))
            .collect();
        assert_eq!(ids, vec!["a2", "a1"], "newest first, own rows only");

        let data = success_data(handle(
            &handler,
            2000,
            method::SEC_EVENTS_LIST,
            json!({"session_id":"s-1"}),
        ));
        assert_eq!(data["items"].as_array().expect("items").len(), 1);
    }

    #[test]
    fn root_filters_plain_session_ids_and_qualifies_only_returned_labels() {
        let (_dir, source) = two_owner_source();
        let handler = QueryHandler::default().with_security_queries(source);

        let data = success_data(handle(&handler, 0, method::SEC_EVENTS_LIST, json!({})));
        assert_eq!(data["total"], 3);
        let data = success_data(handle(
            &handler,
            0,
            method::SEC_EVENTS_LIST,
            json!({"session_id":"s-1"}),
        ));
        assert_eq!(data["total"], 3);
        let items = data["items"].as_array().unwrap();
        assert_eq!(
            items
                .iter()
                .map(|event| event["uid"].as_u64().unwrap())
                .collect::<Vec<_>>(),
            vec![2000, 1000, 1000]
        );
        assert_eq!(
            items
                .iter()
                .map(|event| event["session_id"].as_str().unwrap())
                .collect::<Vec<_>>(),
            vec!["2000_s-1", "1000_s-1", "1000_s-1"]
        );

        let summary = success_data(handle(
            &handler,
            0,
            method::SEC_SUMMARY,
            json!({"session_id":"s-1"}),
        ));
        assert_eq!(summary["total"], 3);
        assert_eq!(summary["affected_sessions"], 2);
        assert_eq!(summary["latest_events"][0]["session_id"], "2000_s-1");

        let event = success_data(handle(
            &handler,
            0,
            method::SEC_EVENTS_GET,
            json!({"event_id":"b1"}),
        ));
        assert_eq!(event["found"], true);
        assert_eq!(event["event"]["uid"], 2000);
        assert_eq!(event["event"]["session_id"], "2000_s-1");

        let groups = success_data(handle(
            &handler,
            0,
            method::SEC_EVENTS_COUNT_BY,
            json!({"session_id":"s-1", "group_by":"session_id"}),
        ));
        assert_eq!(
            groups["items"],
            json!([{ "value":"1000_s-1", "count":2 }, { "value":"2000_s-1", "count":1 }])
        );
        let page = success_data(handle(
            &handler,
            0,
            method::SEC_EVENTS_LIST,
            json!({"session_id":"s-1", "limit":1, "offset":1}),
        ));
        assert_eq!(page["items"][0]["event_id"], "a2");
        assert_eq!(page["items"][0]["session_id"], "1000_s-1");
        assert_eq!(page["total"], 3);
        assert_eq!(page["next_offset"], 2);
    }

    #[test]
    fn identity_parameters_are_rejected_for_every_peer() {
        let (_dir, source) = two_owner_source();
        let handler = QueryHandler::default().with_security_queries(source);

        for uid in [0, 1000] {
            for method in [
                method::SEC_EVENTS_LIST,
                method::SEC_SUMMARY,
                method::SEC_EVENTS_GET,
                method::SEC_EVENTS_COUNT_BY,
            ] {
                for params in [
                    json!({"owner_uid":2000}),
                    json!({"owner_uid":1000}),
                    json!({"uid":uid}),
                    json!({"uid":null}),
                    json!({"owner_uid":null}),
                ] {
                    assert_eq!(
                        error_code_of(handle(&handler, uid, method, params)),
                        "invalid_request"
                    );
                }
            }
        }

        // A numeric prefix is part of the literal session ID, never an owner selector.
        let data = success_data(handle(
            &handler,
            1000,
            method::SEC_EVENTS_LIST,
            json!({"session_id": "2000_s-1"}),
        ));
        assert_eq!(data["total"], json!(0));
    }

    #[test]
    fn a_policy_administrator_still_reads_only_its_own_rows() {
        let (_dir, source) = two_owner_source();
        let handler = QueryHandler::default().with_security_queries(source);

        let admin = Principal::from_authenticated_peer(
            PeerCredentials::new(3000, 100, 7),
            PrincipalRole::PolicyAdministrator,
        );
        let query = method::resolve(method::SEC_EVENTS_LIST).expect("registered method");
        let method::MethodId::Query(query) = query else {
            panic!("not a query method");
        };
        let response = handler.handle(request_id(), &admin, query, json!({}));
        assert_eq!(success_data(response)["total"], 0);
        let response = handler.handle(request_id(), &admin, query, json!({"owner_uid": 1000}));
        assert_eq!(error_code_of(response), "invalid_request");
    }

    #[test]
    fn summary_counts_only_the_callers_rows() {
        let (_dir, source) = two_owner_source();
        let handler = QueryHandler::default().with_security_queries(source);

        let data = success_data(handle(&handler, 1000, method::SEC_SUMMARY, json!({})));
        assert_eq!(data["total"], json!(2));
        assert_eq!(data["affected_sessions"], json!(1));
        assert_eq!(
            data["by_category"],
            json!({"exec": 1, "network": 1}),
            "owner B's exec row must not leak into owner A's buckets"
        );
        assert!(
            data["latest_events"].as_array().expect("events").len() <= 5,
            "latest_limit defaults to five"
        );

        let data = success_data(handle(&handler, 2000, method::SEC_SUMMARY, json!({})));
        assert_eq!(data["total"], json!(1));
    }

    #[test]
    fn get_is_indistinguishable_between_foreign_and_missing() {
        let (_dir, source) = two_owner_source();
        let handler = QueryHandler::default().with_security_queries(source);

        let own = success_data(handle(
            &handler,
            1000,
            method::SEC_EVENTS_GET,
            json!({"event_id": "a1"}),
        ));
        assert_eq!(own["found"], json!(true));
        assert!(own["event"]["details"].is_object(), "get keeps details");

        let foreign = success_data(handle(
            &handler,
            1000,
            method::SEC_EVENTS_GET,
            json!({"event_id": "b1"}),
        ));
        let missing = success_data(handle(
            &handler,
            1000,
            method::SEC_EVENTS_GET,
            json!({"event_id": "no-such-event"}),
        ));
        assert_eq!(foreign["found"], json!(false));
        assert_eq!(missing["found"], json!(false));
        assert_eq!(foreign["event"], json!(null));
        assert_eq!(missing["event"], json!(null));
    }

    #[test]
    fn count_by_applies_the_offset_in_sql_and_delivers_null_buckets() {
        // Rows newest first: a2 (no session, SQL NULL bucket), a1 (session
        // s-1) — both uid 1000; b1 belongs to uid 2000 and stays invisible.
        // Skipping the newest row groups only a1, and the SQL NULL bucket
        // is delivered as `value: null` for the client's v1 `"null"` key.
        let dir = TempDir::new().expect("temp dir");
        let path = dir.path().join("events.db");
        let writer = SqliteEventWriter::new(&path).expect("writer");
        let mut a1 = SecurityEvent::new("sandbox_prehook", "exec", Map::new());
        "a1".clone_into(&mut a1.event_id);
        a1.uid = 1000;
        a1.session_id = Some("s-1".to_owned());
        let mut a2 = SecurityEvent::new("sandbox_prehook", "network", Map::new());
        "a2".clone_into(&mut a2.event_id);
        a2.uid = 1000;
        let mut b1 = SecurityEvent::new("sandbox_prehook", "exec", Map::new());
        "b1".clone_into(&mut b1.event_id);
        b1.uid = 2000;
        for event in [a1, a2, b1] {
            writer.write(&event);
        }
        writer.close_at(1000.0);
        let source = SqliteEventQuerySource::new(&path).expect("source");
        let handler = QueryHandler::default().with_security_queries(source);

        let data = success_data(handle(
            &handler,
            1000,
            method::SEC_EVENTS_COUNT_BY,
            json!({"group_by": "session_id"}),
        ));
        assert_eq!(
            data["items"],
            json!([
                {"value": null, "count": 1},
                {"value": "s-1", "count": 1},
            ])
        );

        let data = success_data(handle(
            &handler,
            1000,
            method::SEC_EVENTS_COUNT_BY,
            json!({"group_by": "session_id", "offset": 1}),
        ));
        assert_eq!(data["items"], json!([{"value": "s-1", "count": 1}]));
    }

    #[test]
    fn count_by_groups_and_sorts_only_own_rows() {
        let (_dir, source) = two_owner_source();
        let handler = QueryHandler::default().with_security_queries(source);

        let data = success_data(handle(
            &handler,
            1000,
            method::SEC_EVENTS_COUNT_BY,
            json!({"group_by": "category"}),
        ));
        assert_eq!(data["group_by"], json!("category"));
        assert_eq!(
            data["items"],
            json!([
                {"value": "exec", "count": 1},
                {"value": "network", "count": 1},
            ])
        );
    }

    #[test]
    fn the_dashboard_projection_matches_v1() {
        let (_dir, source) = two_owner_source();
        let handler = QueryHandler::default().with_security_queries(source);

        // The deny verdict is derived into the row payload, details are
        // dropped by default.
        let data = success_data(handle(
            &handler,
            1000,
            method::SEC_EVENTS_LIST,
            json!({"include_details": false, "limit": 1}),
        ));
        let item = &data["items"][0];
        assert!(item.get("details").is_none(), "details dropped");
        assert_eq!(item["verdict"], json!(null), "a2 carries no verdict");

        let data = success_data(handle(
            &handler,
            1000,
            method::SEC_EVENTS_LIST,
            json!({"include_details": true}),
        ));
        let a1 = data["items"]
            .as_array()
            .expect("items")
            .iter()
            .find(|item| item["event_id"] == json!("a1"))
            .expect("a1 present");
        assert_eq!(a1["verdict"], json!("deny"));
        assert!(a1["details"].is_object());
        assert_eq!(a1["uid"], json!(1000));
        assert_eq!(a1["session_id"], json!("s-1"));
    }

    #[test]
    fn pagination_reports_the_v1_cursor_with_a_pre_pagination_total() {
        let rows: Vec<(&str, u32, &str, Option<&str>)> = (1..=5)
            .map(|index| {
                let id: &str = Box::leak(format!("e{index}").into_boxed_str());
                (id, 1000, "exec", None)
            })
            .collect();
        let (_dir, source) = seeded_source(&rows);
        let handler = QueryHandler::default().with_security_queries(source);

        // The reviewer's scenario: five rows, limit 2, offset 2. The old code
        // folded the offset into the count and reported total=3 with no
        // cursor, dropping the fifth row.
        let data = success_data(handle(
            &handler,
            1000,
            method::SEC_EVENTS_LIST,
            json!({"limit": 2, "offset": 2}),
        ));
        assert_eq!(data["total"], json!(5), "total is the pre-pagination count");
        assert_eq!(data["offset"], json!(2));
        assert_eq!(
            data["next_offset"],
            json!(4),
            "the cursor advances absolutely"
        );

        // A full three-page traversal visits every row exactly once.
        let mut seen: Vec<String> = Vec::new();
        let mut offset = 0_u64;
        for expected_pages in 0..3 {
            let data = success_data(handle(
                &handler,
                1000,
                method::SEC_EVENTS_LIST,
                json!({"limit": 2, "offset": offset}),
            ));
            assert_eq!(data["total"], json!(5), "every page reports the same total");
            for item in data["items"].as_array().expect("items") {
                seen.push(item["event_id"].as_str().expect("id").to_owned());
            }
            let next = data["next_offset"].as_u64();
            if expected_pages < 2 {
                assert_eq!(next, Some(offset + 2));
                offset = next.expect("cursor");
            } else {
                assert_eq!(next, None, "the last page has no cursor");
            }
        }
        assert_eq!(seen.len(), 5);
        seen.sort_unstable();
        assert_eq!(
            seen,
            vec!["e1", "e2", "e3", "e4", "e5"],
            "three pages cover every row exactly once"
        );
    }

    #[test]
    fn the_v1_filter_set_is_honoured() {
        let (_dir, source) = seeded_source(&[
            ("a1", 1000_u32, "exec", Some("deny")),
            ("a2", 1000, "network", None),
            ("b1", 2000, "exec", None),
        ]);
        let handler = QueryHandler::default().with_security_queries(source);

        let data = success_data(handle(
            &handler,
            1000,
            method::SEC_EVENTS_LIST,
            json!({"category": "exec", "verdict": "deny"}),
        ));
        assert_eq!(data["total"], json!(1));
        assert_eq!(data["items"][0]["event_id"], json!("a1"));

        let data = success_data(handle(
            &handler,
            1000,
            method::SEC_EVENTS_LIST,
            json!({"result": "failed"}),
        ));
        assert_eq!(data["total"], json!(0));
    }

    #[test]
    fn invalid_parameters_are_rejected_with_invalid_argument() {
        let (_dir, source) = two_owner_source();
        let handler = QueryHandler::default().with_security_queries(source);

        let cases: Vec<(&str, Value)> = vec![
            (method::SEC_EVENTS_GET, json!({})),
            (method::SEC_EVENTS_GET, json!({"event_id": "  "})),
            (method::SEC_EVENTS_COUNT_BY, json!({})),
            (method::SEC_EVENTS_COUNT_BY, json!({"group_by": "details"})),
            (
                method::SEC_EVENTS_COUNT_BY,
                json!({"group_by": "category", "limit": 10}),
            ),
            // `offset` is accepted (it skips rows in SQL before
            // grouping, as v1's reader did); only its i64 overflow is
            // rejected.
            (
                method::SEC_EVENTS_COUNT_BY,
                json!({"group_by": "category", "offset": 9_223_372_036_854_775_808_u64}),
            ),
            (method::SEC_EVENTS_LIST, json!({"limit": 0})),
            (method::SEC_EVENTS_LIST, json!({"limit": 1001})),
            (method::SEC_EVENTS_LIST, json!({"result": "exploded"})),
            (method::SEC_EVENTS_LIST, json!({"since": "not a timestamp"})),
            (
                method::SEC_EVENTS_LIST,
                json!({"since": "2026-01-02T00:00:00+00:00", "until": "2026-01-01T00:00:00+00:00"}),
            ),
            (
                method::SEC_EVENTS_LIST,
                json!({"since": "2026-01-01T00:00:00+00:00", "start_ns": 1}),
            ),
            (method::SEC_SUMMARY, json!({"latest_limit": 51})),
            (method::SEC_SUMMARY, json!({"latest_limit": 0})),
        ];
        for (method_name, params) in cases {
            let response = handle(&handler, 1000, method_name, params);
            assert_eq!(
                error_code_of(response),
                "invalid_argument",
                "{method_name} must reject invalid parameters"
            );
        }
    }

    #[test]
    fn the_full_v1_offset_range_is_accepted() {
        let (_dir, source) = two_owner_source();
        let handler = QueryHandler::default().with_security_queries(source);

        // u32::MAX is a legal offset now; beyond i64::MAX is not.
        let data = success_data(handle(
            &handler,
            1000,
            method::SEC_EVENTS_LIST,
            json!({"offset": 4_294_967_295_u64}),
        ));
        assert_eq!(data["total"], json!(2));
        assert_eq!(data["next_offset"], json!(null));

        let response = handle(
            &handler,
            1000,
            method::SEC_EVENTS_LIST,
            json!({"offset": 9_223_372_036_854_775_808_u64}),
        );
        assert_eq!(error_code_of(response), "invalid_argument");
    }

    #[test]
    fn unknown_or_foreign_parameters_are_rejected_with_invalid_request() {
        let (_dir, source) = two_owner_source();
        let handler = QueryHandler::default().with_security_queries(source);

        let response = handle(
            &handler,
            1000,
            method::SEC_EVENTS_LIST,
            json!({"ownerUid": 2000}),
        );
        assert_eq!(
            error_code_of(response),
            "invalid_request",
            "a caller must not be able to name a scope"
        );

        let response = handle(
            &handler,
            1000,
            method::SEC_EVENTS_LIST,
            json!({"limit": true}),
        );
        assert_eq!(error_code_of(response), "invalid_request");
    }

    #[test]
    fn epoch_nanosecond_bounds_are_accepted() {
        let (_dir, source) = two_owner_source();
        let handler = QueryHandler::default().with_security_queries(source);

        // 2026-01-01T00:00:00Z in epoch nanoseconds: everything is after it.
        let start_ns = 1_767_225_600_000_000_000_u64;
        let data = success_data(handle(
            &handler,
            1000,
            method::SEC_EVENTS_LIST,
            json!({"start_ns": start_ns}),
        ));
        assert_eq!(data["total"], json!(2));
    }

    #[test]
    fn a_missing_store_degrades_to_empty_results_not_errors() {
        let dir = TempDir::new().expect("temp dir");
        let path: std::path::PathBuf = dir.path().join("absent.db");
        let handler = QueryHandler::default().with_security_queries(
            SqliteEventQuerySource::new(&path).expect("source over a missing store"),
        );

        let data = success_data(handle(&handler, 1000, method::SEC_SUMMARY, json!({})));
        assert_eq!(data["total"], json!(0));
        let data = success_data(handle(
            &handler,
            1000,
            method::SEC_EVENTS_GET,
            json!({"event_id": "a1"}),
        ));
        assert_eq!(data["found"], json!(false));
    }

    #[test]
    fn a_corrupt_store_is_an_unavailable_error_not_an_empty_answer() {
        let dir = TempDir::new().expect("temp dir");
        let path = dir.path().join("events.db");
        std::fs::write(&path, b"definitely not a sqlite database").expect("seed bytes");
        let handler = QueryHandler::default().with_security_queries(
            SqliteEventQuerySource::new(&path).expect("source opens lazily"),
        );

        for (method_name, params) in [
            (method::SEC_EVENTS_LIST, json!({})),
            (method::SEC_SUMMARY, json!({})),
            (method::SEC_EVENTS_GET, json!({"event_id": "a1"})),
            (method::SEC_EVENTS_COUNT_BY, json!({"group_by": "category"})),
        ] {
            let response = handle(&handler, 1000, method_name, params);
            assert_eq!(
                error_code_of(response),
                "unavailable",
                "{method_name} must not report success over a corrupt store"
            );
        }
    }
}
