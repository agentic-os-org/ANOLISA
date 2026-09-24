//! Wire projection for single-record ingestion.
use asc_daemon_core::{ObservabilityService, ObservabilityWriteError};
use asc_daemon_protocol::{DaemonResponse, ObservabilityRecordParams, RequestId, error_code};
use asc_daemon_service::DispatchControl;
use serde_json::Value;

pub(crate) fn handle(
    id: RequestId,
    control: &DispatchControl,
    service: Option<&ObservabilityService>,
    params: Value,
) -> DaemonResponse {
    let Ok(params) = serde_json::from_value::<ObservabilityRecordParams>(params) else {
        return DaemonResponse::error(
            id,
            error_code::INVALID_ARGUMENT,
            "invalid observability parameters",
        );
    };
    if control.is_cancelled() {
        return DaemonResponse::error(
            id,
            error_code::DEADLINE_EXCEEDED,
            "request dispatch deadline expired",
        );
    }
    let Some(service) = service else {
        return DaemonResponse::error(
            id,
            error_code::UNAVAILABLE,
            "observability ingestion unavailable",
        );
    };
    match service.record(&params.hook, &params.observed_at, &params.metrics) {
        Ok(()) => DaemonResponse::success(id, serde_json::json!({})),
        Err(ObservabilityWriteError::Invalid(_)) => DaemonResponse::error(
            id,
            error_code::INVALID_ARGUMENT,
            "invalid observability record or required metadata",
        ),
        Err(ObservabilityWriteError::Storage) => DaemonResponse::error(
            id,
            error_code::INTERNAL,
            "failed to write observability record; partial write is possible",
        ),
    }
}
