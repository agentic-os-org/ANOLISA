//! Observability query wire validation and safe projection over the core service.

use asc_daemon_core::{
    PeerCredentials,
    query::{
        ObservabilityQueryService, QueryControl, QueryError, QueryPage, QueryScope, QueryWindow,
    },
};
use asc_daemon_protocol::{
    DaemonResponse, ObservabilityQueryParams, RequestId, error_code,
    method::ObservabilityQueryMethod,
};
use asc_daemon_service::DispatchControl;
use asc_security_events::timestamp::{NaivePolicy, normalize_iso_to_utc_iso, utc_iso_to_epoch};
use serde_json::Value;

impl crate::QueryHandler {
    /// Serves one observability query using authenticated peer credentials.
    pub fn handle_observability(
        &self,
        id: RequestId,
        peer: PeerCredentials,
        control: &DispatchControl,
        method: ObservabilityQueryMethod,
        raw: Value,
    ) -> DaemonResponse {
        let result = execute(peer, control, self.observability.as_ref(), method, raw);
        match result {
            Ok(value) => DaemonResponse::success(id, value),
            Err(error) => {
                let code = match error {
                    QueryError::InvalidArgument => error_code::INVALID_ARGUMENT,
                    QueryError::PermissionDenied => error_code::PERMISSION_DENIED,
                    QueryError::Unavailable => error_code::UNAVAILABLE,
                    QueryError::DeadlineExceeded => error_code::DEADLINE_EXCEEDED,
                    QueryError::ResourceExhausted => error_code::RESOURCE_EXHAUSTED,
                    QueryError::Internal => error_code::INTERNAL,
                };
                DaemonResponse::error(id, code, &error.to_string())
            }
        }
    }
}

fn execute(
    peer: PeerCredentials,
    control: &DispatchControl,
    service: Option<&ObservabilityQueryService>,
    method: ObservabilityQueryMethod,
    raw: Value,
) -> Result<Value, QueryError> {
    let params: ObservabilityQueryParams =
        serde_json::from_value(raw).map_err(|_| QueryError::InvalidArgument)?;
    let scope = QueryScope::from_peer(peer);
    let timeline = method == ObservabilityQueryMethod::Timeline;
    let page = QueryPage {
        limit: params.limit.unwrap_or(if timeline { 1000 } else { 100 }),
        offset: params.offset.unwrap_or(0),
    };
    if !(1..=1000).contains(&page.limit) || page.offset < 0 {
        return Err(QueryError::InvalidArgument);
    }
    let window = QueryWindow {
        since: bound(params.since.as_deref(), params.start_ns)?,
        until: bound(params.until.as_deref(), params.end_ns)?,
    };
    if window.since.zip(window.until).is_some_and(|(a, b)| a > b) {
        return Err(QueryError::InvalidArgument);
    }
    if !timeline && (params.run_id.is_some() || params.include_security.is_some()) {
        return Err(QueryError::InvalidArgument);
    }
    match method {
        ObservabilityQueryMethod::Sessions if params.session_id.is_some() => {
            required(params.session_id.as_deref())?;
        }
        ObservabilityQueryMethod::Runs | ObservabilityQueryMethod::Timeline => {
            required(params.session_id.as_deref())?;
        }
        ObservabilityQueryMethod::Sessions => {}
    }
    if timeline {
        required(params.run_id.as_deref())?;
    }
    let signal = control.clone();
    let control = QueryControl::new(control.deadline(), move || signal.is_cancelled());
    control.check()?;
    let service = service.ok_or(QueryError::Unavailable)?;
    match method {
        ObservabilityQueryMethod::Sessions => {
            service.sessions(scope, params.session_id.as_deref(), window, page, &control)
        }
        ObservabilityQueryMethod::Runs => service.runs(
            scope,
            required(params.session_id.as_deref())?,
            window,
            page,
            &control,
        ),
        ObservabilityQueryMethod::Timeline => service.timeline(
            scope,
            required(params.session_id.as_deref())?,
            required(params.run_id.as_deref())?,
            window,
            page,
            params.include_security.unwrap_or(true),
            &control,
        ),
    }
}

fn required(value: Option<&str>) -> Result<&str, QueryError> {
    value
        .filter(|s| !s.trim().is_empty())
        .ok_or(QueryError::InvalidArgument)
}

fn bound(iso: Option<&str>, nanos: Option<u64>) -> Result<Option<f64>, QueryError> {
    if iso.is_some() && nanos.is_some() {
        return Err(QueryError::InvalidArgument);
    }
    if let Some(iso) = iso {
        let value = normalize_iso_to_utc_iso(iso, "time", NaivePolicy::Local)
            .map_err(|_| QueryError::InvalidArgument)?;
        return utc_iso_to_epoch(&value, "time")
            .map(Some)
            .map_err(|_| QueryError::InvalidArgument);
    }
    // Each split operand is exactly representable; epoch seconds retain SQLite's f64 precision.
    #[allow(clippy::cast_precision_loss)]
    Ok(nanos.map(|n| (n / 1_000_000_000) as f64 + (n % 1_000_000_000) as f64 / 1e9))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::time::{Duration, Instant};

    /// QRY-003: executable query-contract coverage.
    #[test]
    fn rejects_identity_overrides_and_invalid_filters_before_storage_access() {
        let control = DispatchControl::new(Instant::now() + Duration::from_secs(5));
        for params in [
            json!({"uid":1000}),
            json!({"owner_uid":1000}),
            json!({"uid":null}),
            json!({"role":"root"}),
            json!({"scope":"all"}),
            json!({"limit":0}),
            json!({"limit":1001}),
            json!({"offset":-1}),
            json!({"offset":9_223_372_036_854_775_808_u64}),
            json!({"uid":4_294_967_296_u64}),
            json!({"uid":true}),
            json!({"since":"bad"}),
            json!({"since":"2030-01-01T00:00:00Z","start_ns":1}),
            json!({"start_ns":2,"end_ns":1}),
            json!({"session_id":" "}),
        ] {
            assert!(
                matches!(
                    execute(
                        PeerCredentials::new(1000, 1000, 1),
                        &control,
                        None,
                        ObservabilityQueryMethod::Sessions,
                        params.clone()
                    ),
                    Err(QueryError::InvalidArgument)
                ),
                "{params}"
            );
        }
        assert!(matches!(
            execute(
                PeerCredentials::new(1000, 1000, 1),
                &control,
                None,
                ObservabilityQueryMethod::Sessions,
                json!({"uid":0})
            ),
            Err(QueryError::InvalidArgument)
        ));
        for method in [
            ObservabilityQueryMethod::Runs,
            ObservabilityQueryMethod::Timeline,
        ] {
            assert!(matches!(
                execute(
                    PeerCredentials::new(0, 0, 1),
                    &control,
                    None,
                    method,
                    json!({})
                ),
                Err(QueryError::InvalidArgument)
            ));
        }
    }

    /// QRY-007: sessions accepts an optional exact identifier at the wire boundary.
    #[test]
    fn sessions_accepts_exact_identifier_before_storage_access() {
        let control = DispatchControl::new(Instant::now() + Duration::from_secs(5));
        assert!(matches!(
            execute(
                PeerCredentials::new(1000, 1000, 1),
                &control,
                None,
                ObservabilityQueryMethod::Sessions,
                json!({"session_id":" s "})
            ),
            Err(QueryError::Unavailable)
        ));
    }

    #[test]
    fn nanos_are_seconds_and_identifiers_retain_significant_whitespace() {
        let nanos = bound(None, Some(1_893_456_000_123_000_000))
            .unwrap()
            .unwrap();
        let iso = bound(Some("2030-01-01T00:00:00.123Z"), None)
            .unwrap()
            .unwrap();
        assert!((nanos - iso).abs() < 0.000_001);
        assert_eq!(required(Some(" s ")).unwrap(), " s ");
    }
}
