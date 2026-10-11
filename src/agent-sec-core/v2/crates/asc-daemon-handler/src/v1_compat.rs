//! V1 response envelopes with legacy observability payload projection.

use std::io::Write;
use std::time::Instant;

use asc_daemon_core::PeerCredentials;
use asc_daemon_protocol::{DaemonRequest, DaemonResponse, V1Request, error_code, method};
use asc_daemon_service::{DispatchError, DispatchRequest, ResponseDisposition};
use serde_json::{Value, json};

use crate::dispatcher::{DaemonDispatcher, new_request_id};

pub(crate) fn dispatch(
    dispatcher: &DaemonDispatcher,
    request: &DispatchRequest,
    response: &mut dyn Write,
) -> Option<Result<ResponseDisposition, DispatchError>> {
    let value: Value = serde_json::from_slice(&request.payload).ok()?;
    if value.get("caller")?.as_str()? != "agentsight" {
        return None;
    }
    let legacy = match handle(dispatcher, request) {
        DaemonResponse::Success(result) => json!({
            "request_id":result.request_id, "ok":true, "data":result.result,
            "stdout":"", "stderr":"", "exit_code":0
        }),
        DaemonResponse::Error(result) => {
            let code = match result.error.code.as_str() {
                error_code::INVALID_REQUEST | error_code::INVALID_ARGUMENT => "bad_request",
                error_code::DEADLINE_EXCEEDED => "timeout",
                error_code::RESOURCE_EXHAUSTED => "payload_too_large",
                error_code::INTERNAL => "internal_error",
                code => code,
            };
            json!({"request_id":result.request_id,"ok":false,"data":{},
                "stdout":"","stderr":result.error.message(),"exit_code":1,
                "error":{"code":code,"message":result.error.message()}})
        }
    };
    Some(
        serde_json::to_writer(response, &legacy)
            .map(|()| ResponseDisposition::Send)
            .map_err(|_| DispatchError),
    )
}

fn handle(dispatcher: &DaemonDispatcher, request: &DispatchRequest) -> DaemonResponse {
    let id = new_request_id();
    let Ok(decoded) = serde_json::from_slice::<V1Request>(&request.payload) else {
        return DaemonResponse::error(id, error_code::INVALID_REQUEST, "query envelope is invalid");
    };
    if request.payload.len() >= asc_daemon_protocol::BUSINESS_FRAME_BYTES {
        return DaemonResponse::error(
            id,
            error_code::RESOURCE_EXHAUSTED,
            "query frame exceeds the configured limit",
        );
    }
    if decoded
        .timeout_ms
        .is_some_and(|ms| !(1..=300_000).contains(&ms))
    {
        return DaemonResponse::error(
            id,
            error_code::INVALID_ARGUMENT,
            "timeout_ms must be between 1 and 300000",
        );
    }
    if request.control.is_cancelled() || Instant::now() >= request.control.deadline() {
        return DaemonResponse::error(id, error_code::DEADLINE_EXCEEDED, "query deadline expired");
    }
    let method = decoded.method.clone();
    let result = dispatcher.handle_with_control(
        id,
        PeerCredentials::new(request.peer.uid(), request.peer.gid(), request.peer.pid()),
        &request.control,
        DaemonRequest {
            method: decoded.method,
            params: decoded.params,
            trace_context: None,
            compatibility: None,
        },
    );
    match result {
        DaemonResponse::Success(mut result) => {
            project(&method, &mut result.result);
            DaemonResponse::Success(result)
        }
        error @ DaemonResponse::Error(_) => error,
    }
}

fn remove(value: &mut Value, keys: &[&str]) {
    if let Some(object) = value.as_object_mut() {
        for key in keys {
            object.remove(*key);
        }
    }
}

fn project(method: &str, data: &mut Value) {
    if !method::OBS_QUERY_METHODS.contains(&method) {
        return;
    }
    let keys: &[&str] = match method {
        method::OBS_RUNS_LIST => &["uid"],
        method::OBS_TIMELINE_GET => &["uid", "total", "next_offset"],
        _ => &[],
    };
    remove(data, keys);
    if let Some(items) = data.get_mut("items").and_then(Value::as_array_mut) {
        for item in items {
            remove(item, &["uid", "security_by_category_result"]);
            if let Some(context) = item.get_mut("observability") {
                remove(context, &["uid"]);
            }
        }
    }
}
