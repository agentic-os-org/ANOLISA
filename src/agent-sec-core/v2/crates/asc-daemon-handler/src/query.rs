//! Owner-scoped security-event query projection over the daemon's read store.
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
//! * **Cross-owner audit is absent on purpose.** The server assigns no
//!   auditor role yet, so no principal — administrator or not — reads
//!   another owner's rows through these methods (issue #6608).

use std::path::Path;

use asc_daemon_core::Principal;
use asc_daemon_protocol::{
    DaemonResponse, MAX_DAEMON_ERROR_MESSAGE_BYTES, RequestId, SecQueryParams, error_code, method,
};
use asc_persistence_sqlite::QueryScope;
use asc_persistence_sqlite::security_events::{
    EventFilters, GroupCounts, SqliteEventReader, VALID_GROUP_FIELDS,
};
use asc_security_events::timestamp::{NaivePolicy, normalize_iso_to_utc_iso, utc_iso_to_epoch};
use asc_security_events::{SecurityEvent, SecurityEventsSummary, extract_verdict};
use asc_sqlite_kernel::KernelError;
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

/// Read-only security-event queries one daemon can serve, scoped per owner.
///
/// The port keeps the handler free of storage decisions; the daemon
/// composition root binds it to the same database the writers use.
pub trait SecurityEventQueries: Send + Sync {
    /// Returns the aggregates and newest rows of one owner scope.
    fn summary(
        &self,
        filters: &EventFilters,
        scope: &QueryScope,
        latest_limit: u32,
    ) -> SecurityEventsSummary;
    /// Returns one page of one owner scope's rows, newest first.
    fn list(
        &self,
        filters: &EventFilters,
        scope: &QueryScope,
        limit: u32,
        offset: u32,
    ) -> Vec<SecurityEvent>;
    /// Returns the number of remaining rows of one owner scope.
    fn count(&self, filters: &EventFilters, scope: &QueryScope, offset: u32) -> u64;
    /// Returns grouped counts of one owner scope.
    ///
    /// # Errors
    ///
    /// Returns [`KernelError::Malformed`] when `group_field` is outside the
    /// v1 allowlist.
    fn count_by(
        &self,
        group_field: &str,
        filters: &EventFilters,
        scope: &QueryScope,
        offset: u32,
    ) -> Result<GroupCounts, KernelError>;
    /// Returns one row of one owner scope by id, or `None`.
    fn get(&self, event_id: &str, scope: &QueryScope) -> Option<SecurityEvent>;
}

/// [`SecurityEventQueries`] over the daemon's security-event database.
///
/// The reader opens its own read-only connection and survives a replaced
/// database file by inode check; a database that does not exist yet degrades
/// to empty results, which is the correct answer before the first write.
pub struct SqliteEventQuerySource {
    reader: SqliteEventReader,
}

impl SqliteEventQuerySource {
    /// Opens the source over the database at `path`.
    ///
    /// # Errors
    ///
    /// Returns [`KernelError`] when the path cannot be normalized.
    pub fn new(path: &Path) -> Result<Self, KernelError> {
        Ok(Self {
            reader: SqliteEventReader::new(path)?,
        })
    }
}

impl SecurityEventQueries for SqliteEventQuerySource {
    fn summary(
        &self,
        filters: &EventFilters,
        scope: &QueryScope,
        latest_limit: u32,
    ) -> SecurityEventsSummary {
        self.reader.summary(filters, scope, latest_limit)
    }

    fn list(
        &self,
        filters: &EventFilters,
        scope: &QueryScope,
        limit: u32,
        offset: u32,
    ) -> Vec<SecurityEvent> {
        self.reader.query(filters, scope, limit, offset)
    }

    fn count(&self, filters: &EventFilters, scope: &QueryScope, offset: u32) -> u64 {
        self.reader.count(filters, scope, offset)
    }

    fn count_by(
        &self,
        group_field: &str,
        filters: &EventFilters,
        scope: &QueryScope,
        offset: u32,
    ) -> Result<GroupCounts, KernelError> {
        self.reader.count_by(group_field, filters, scope, offset)
    }

    fn get(&self, event_id: &str, scope: &QueryScope) -> Option<SecurityEvent> {
        self.reader.get(event_id, scope)
    }
}

/// Protocol adapter for the `sec.*` query family.
pub struct SecurityQueryHandler {
    source: Option<Box<dyn SecurityEventQueries>>,
}

impl SecurityQueryHandler {
    /// Binds the handler to no store.
    ///
    /// Every `sec.*` method is then rejected with `unavailable`, so an
    /// assembly that never binds a store fails closed instead of serving
    /// queries from an arbitrary database.
    pub fn unconfigured() -> Self {
        Self { source: None }
    }

    /// Binds the handler to one query source.
    pub fn new(source: impl SecurityEventQueries + 'static) -> Self {
        Self {
            source: Some(Box::new(source)),
        }
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
        // The owner scope is derived once, from transport-authenticated
        // evidence only. Nothing decoded from the request participates.
        let scope = QueryScope::Owner(principal.peer().uid());
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
        let summary = source.summary(&filters, &scope, latest_limit);
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
        let items = source.list(&filters, &scope, limit, offset);
        let total = source.count(&filters, &scope, offset);
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
        let event = source.get(event_id, &scope);
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
        if params.limit.is_some() || params.offset.is_some() {
            return invalid_parameters(request_id);
        }
        let Some(group_by) = non_empty(params.group_by.as_deref()) else {
            return invalid_parameters(request_id);
        };
        if !VALID_GROUP_FIELDS.contains(&group_by) {
            return invalid_parameters(request_id);
        }
        let Ok(filters) = event_filters(params) else {
            return invalid_parameters(request_id);
        };
        match source.count_by(group_by, &filters, &scope, 0) {
            Ok(groups) => DaemonResponse::success(
                request_id,
                json!({
                    "group_by": group_by,
                    "items": count_items(&groups),
                }),
            ),
            Err(_) => invalid_parameters(request_id),
        }
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
fn list_filters(params: &SecQueryParams) -> Result<(EventFilters, u32, u32, bool), ()> {
    if params.group_by.is_some() || params.event_id.is_some() || params.latest_limit.is_some() {
        return Err(());
    }
    let filters = event_filters(params)?;
    let limit = bounded(params.limit, DEFAULT_LIMIT, 1, MAX_LIMIT)?;
    let offset = bounded(params.offset, 0, 0, u64::from(u32::MAX))?;
    let include_details = params.include_details.unwrap_or(false);
    Ok((
        filters,
        u32::try_from(limit).expect("bounded to 1000"),
        u32::try_from(offset).expect("bounded to u32::MAX"),
        include_details,
    ))
}

/// Builds the repository filter set from the shared v1 parameter names.
fn event_filters(params: &SecQueryParams) -> Result<EventFilters, ()> {
    if let Some(result) = non_empty(params.result.as_deref()) {
        if !EVENT_RESULTS.contains(&result) {
            return Err(());
        }
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
    if let (Some(since), Some(until)) = (since, until) {
        if since > until {
            return Err(());
        }
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
fn next_offset(offset: u32, limit: u64, returned: u64, total: u64) -> Option<u64> {
    let offset = u64::from(offset);
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
        if let Some(skill_dir) =
            request.and_then(|request| first_non_empty_string(request.get("skill_dir")))
        {
            skill_name = Path::new(&skill_dir)
                .file_name()
                .and_then(|name| name.to_str())
                .map(str::to_owned)
                .filter(|name| !name.is_empty());
        }
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
fn count_items(groups: &GroupCounts) -> Vec<Value> {
    let mut items: Vec<Value> = groups
        .iter()
        .filter_map(|(key, count)| {
            key.as_deref()
                .filter(|key| !key.is_empty())
                .map(|key| json!({"value": key, "count": count}))
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

fn bounded_message(message: &str) -> String {
    if message.len() > MAX_DAEMON_ERROR_MESSAGE_BYTES {
        "request parameters are invalid".to_owned()
    } else {
        message.to_owned()
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;
    use asc_daemon_core::{PeerCredentials, PrincipalRole};
    use asc_persistence_sqlite::security_events::SqliteEventWriter;
    use serde_json::json;
    use tempfile::TempDir;

    fn seeded_source() -> (TempDir, SqliteEventQuerySource) {
        let dir = TempDir::new().expect("temp dir");
        let path = dir.path().join("events.db");
        let writer = SqliteEventWriter::new(&path).expect("writer");
        for (id, uid, category, verdict) in [
            ("a1", 1000_u32, "exec", Some("deny")),
            ("a2", 1000, "network", None),
            ("b1", 2000, "exec", Some("allow")),
        ] {
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
        handler: &SecurityQueryHandler,
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
    fn one_owner_never_sees_another_owners_rows() {
        let (_dir, source) = seeded_source();
        let handler = SecurityQueryHandler::new(source);

        let data = success_data(handle(&handler, 1000, method::SEC_EVENTS_LIST, json!({})));
        let ids: Vec<&str> = data["items"]
            .as_array()
            .expect("items array")
            .iter()
            .map(|item| item["event_id"].as_str().expect("id"))
            .collect();
        assert_eq!(ids, vec!["a2", "a1"], "newest first, own rows only");

        let data = success_data(handle(&handler, 2000, method::SEC_EVENTS_LIST, json!({})));
        assert_eq!(data["items"].as_array().expect("items").len(), 1);
    }

    #[test]
    fn summary_counts_only_the_callers_rows() {
        let (_dir, source) = seeded_source();
        let handler = SecurityQueryHandler::new(source);

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
        let (_dir, source) = seeded_source();
        let handler = SecurityQueryHandler::new(source);

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
    fn count_by_groups_and_sorts_only_own_rows() {
        let (_dir, source) = seeded_source();
        let handler = SecurityQueryHandler::new(source);

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
        let (_dir, source) = seeded_source();
        let handler = SecurityQueryHandler::new(source);

        // The deny verdict is derived into the row payload, details are
        // dropped by default, and a skill-ledger event gains command and
        // skill_name.
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
    fn pagination_reports_the_v1_cursor() {
        let (_dir, source) = seeded_source();
        let handler = SecurityQueryHandler::new(source);

        let data = success_data(handle(
            &handler,
            1000,
            method::SEC_EVENTS_LIST,
            json!({"limit": 1, "offset": 0}),
        ));
        assert_eq!(data["total"], json!(2));
        assert_eq!(data["limit"], json!(1));
        assert_eq!(data["offset"], json!(0));
        assert_eq!(data["next_offset"], json!(1));

        let data = success_data(handle(
            &handler,
            1000,
            method::SEC_EVENTS_LIST,
            json!({"limit": 1, "offset": 1}),
        ));
        assert_eq!(
            data["next_offset"],
            json!(null),
            "the last page has no cursor"
        );
    }

    #[test]
    fn the_v1_filter_set_is_honoured() {
        let (_dir, source) = seeded_source();
        let handler = SecurityQueryHandler::new(source);

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
        let (_dir, source) = seeded_source();
        let handler = SecurityQueryHandler::new(source);

        let cases: Vec<(&str, Value)> = vec![
            (method::SEC_EVENTS_GET, json!({})),
            (method::SEC_EVENTS_GET, json!({"event_id": "  "})),
            (method::SEC_EVENTS_COUNT_BY, json!({})),
            (method::SEC_EVENTS_COUNT_BY, json!({"group_by": "details"})),
            (
                method::SEC_EVENTS_COUNT_BY,
                json!({"group_by": "category", "limit": 10}),
            ),
            (
                method::SEC_EVENTS_COUNT_BY,
                json!({"group_by": "category", "offset": 10}),
            ),
            (method::SEC_EVENTS_LIST, json!({"limit": 0})),
            (method::SEC_EVENTS_LIST, json!({"limit": 1001})),
            (
                method::SEC_EVENTS_LIST,
                json!({"offset": 5_000_000_000_u64}),
            ),
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
    fn unknown_or_foreign_parameters_are_rejected_with_invalid_request() {
        let (_dir, source) = seeded_source();
        let handler = SecurityQueryHandler::new(source);

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
        let (_dir, source) = seeded_source();
        let handler = SecurityQueryHandler::new(source);

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
        let path: PathBuf = dir.path().join("absent.db");
        let handler = SecurityQueryHandler::new(
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
}
