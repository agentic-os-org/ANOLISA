//! Protocol response projection for transport-owned rejections.

use std::io::Write;

use asc_daemon_protocol::{DaemonResponse, error_code};
use asc_daemon_service::{
    DispatchError, RejectedRequest, RejectionEncoder, RejectionReason, ResponseDisposition,
};

use crate::dispatcher::{new_request_id, write_response};

/// Protocol-only projection for transport-owned failures.
#[derive(Debug, Default, Clone, Copy)]
pub struct JsonRejectionEncoder;

impl RejectionEncoder for JsonRejectionEncoder {
    fn encode_rejection(
        &self,
        request: RejectedRequest,
        response: &mut dyn Write,
    ) -> Result<ResponseDisposition, DispatchError> {
        let (code, message) = project_rejection(request.reason);
        write_response(
            response,
            &DaemonResponse::<serde_json::Value>::error(new_request_id(), code, message),
        )
    }
}

fn project_rejection(reason: RejectionReason) -> (&'static str, &'static str) {
    match reason {
        RejectionReason::Busy => (
            error_code::RESOURCE_EXHAUSTED,
            "daemon connection capacity is exhausted",
        ),
        RejectionReason::ShuttingDown => (error_code::UNAVAILABLE, "daemon is shutting down"),
        RejectionReason::RequestReadTimeout => (
            error_code::DEADLINE_EXCEEDED,
            "request read deadline expired",
        ),
        RejectionReason::RequestFrameTooLarge => (
            error_code::RESOURCE_EXHAUSTED,
            "request frame exceeds the configured limit",
        ),
        RejectionReason::EmptyRequest => (error_code::INVALID_REQUEST, "request frame is empty"),
        RejectionReason::DispatchFailed => (error_code::INTERNAL, "request dispatch failed"),
        RejectionReason::DispatchTimedOut => (
            error_code::DEADLINE_EXCEEDED,
            "request dispatch deadline expired",
        ),
        RejectionReason::ResponseFrameTooLarge => (
            error_code::RESOURCE_EXHAUSTED,
            "response frame exceeds the configured limit",
        ),
        RejectionReason::InvalidResponseFrame => {
            (error_code::INTERNAL, "daemon produced an invalid response")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transport_failures_have_stable_protocol_categories() {
        assert_eq!(
            project_rejection(RejectionReason::Busy).0,
            error_code::RESOURCE_EXHAUSTED
        );
        assert_eq!(
            project_rejection(RejectionReason::DispatchTimedOut).0,
            error_code::DEADLINE_EXCEEDED
        );
        assert_eq!(
            project_rejection(RejectionReason::DispatchFailed).0,
            error_code::INTERNAL
        );
    }
}
