// SPDX-License-Identifier: Apache-2.0
//! Local errors for the daemon binary and HTTP API.
//!
//! Wraps [`blaze_core::BlazeError`] so the daemon can additionally
//! surface I/O, hyper, and CLI-side failures without expanding the
//! public core error enum.

use std::path::PathBuf;

use thiserror::Error;

pub type Result<T> = std::result::Result<T, BlazeDaemonError>;

#[derive(Debug, Error)]
pub(crate) enum BlazeDaemonError {
    #[error("core error: {0}")]
    Core(#[from] blaze_core::BlazeError),

    #[error(transparent)]
    Guest(#[from] crate::guest::GuestError),

    #[error("io error: {0}")]
    Io(#[from] std::io::Error),

    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),

    #[error("toml error: {0}")]
    Toml(#[from] toml::de::Error),

    #[error("hyper http error: {0}")]
    HyperHttp(#[from] hyper::http::Error),

    #[error("hyper protocol error: {0}")]
    Hyper(#[from] hyper::Error),

    #[error(
        "could not connect to blaze daemon at {socket}: {source}\nIs the daemon running? Try: blazed daemon start --foreground"
    )]
    #[allow(dead_code)] // Constructed by client code; kept for future use.
    SocketConnect {
        socket: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("daemon returned status {status}: {body}")]
    #[allow(dead_code)] // Constructed by client code; kept for future use.
    HttpStatus { status: u16, body: String },

    #[error("invalid request: {0}")]
    BadRequest(String),

    #[error("not found: {0}")]
    NotFound(String),

    #[error("unsupported operation: {0}")]
    UnsupportedOperation(String),

    #[error("conflict: {0}")]
    Conflict(String),

    #[error("service unavailable: {0}")]
    ServiceUnavailable(String),

    #[error("request body too large: {actual} bytes exceeds {limit}")]
    PayloadTooLarge { actual: u64, limit: usize },

    #[error("operation requires recovery: {0}")]
    RecoveryRequired(String),

    #[error("internal error: {0}")]
    Internal(String),
}

impl BlazeDaemonError {
    /// Stable machine-readable code for errors that callers must branch on.
    pub fn api_code(&self) -> Option<&'static str> {
        match self {
            BlazeDaemonError::Guest(crate::guest::GuestError::Io(_)) => {
                Some("guest_transport_error")
            }
            BlazeDaemonError::Guest(crate::guest::GuestError::Json(_))
            | BlazeDaemonError::Guest(crate::guest::GuestError::Protocol(_)) => {
                Some("guest_response_invalid")
            }
            BlazeDaemonError::Guest(crate::guest::GuestError::InvalidArgument(_)) => {
                Some("guest_invalid_request")
            }
            BlazeDaemonError::Guest(crate::guest::GuestError::Timeout(_)) => Some("guest_timeout"),
            BlazeDaemonError::Guest(crate::guest::GuestError::OutcomeUnknown(_)) => {
                Some("guest_outcome_unknown")
            }
            BlazeDaemonError::Guest(crate::guest::GuestError::Rejected(_)) => {
                Some("guest_rejected")
            }
            BlazeDaemonError::Guest(crate::guest::GuestError::PayloadTooLarge { .. }) => {
                Some("guest_request_too_large")
            }
            BlazeDaemonError::Guest(crate::guest::GuestError::ResponseTooLarge { .. }) => {
                Some("guest_response_too_large")
            }
            BlazeDaemonError::Guest(crate::guest::GuestError::Cancelled) => Some("guest_cancelled"),
            _ => None,
        }
    }

    /// HTTP status code that should accompany this error in API responses.
    pub fn status_code(&self) -> u16 {
        match self {
            BlazeDaemonError::BadRequest(_) => 400,
            BlazeDaemonError::NotFound(_) => 404,
            BlazeDaemonError::UnsupportedOperation(_) => 501,
            BlazeDaemonError::Conflict(_) => 409,
            BlazeDaemonError::ServiceUnavailable(_) => 503,
            BlazeDaemonError::PayloadTooLarge { .. } => 413,
            BlazeDaemonError::RecoveryRequired(_) => 500,
            BlazeDaemonError::HttpStatus { status, .. } => *status,
            BlazeDaemonError::Core(blaze_core::BlazeError::PolicyEvalError { .. })
            | BlazeDaemonError::Core(blaze_core::BlazeError::InvalidStateTransition { .. }) => 422,
            BlazeDaemonError::Core(blaze_core::BlazeError::OperationInProgress { .. }) => 409,
            BlazeDaemonError::Core(blaze_core::BlazeError::BackendUnavailable { .. }) => 503,
            BlazeDaemonError::Guest(crate::guest::GuestError::InvalidArgument(_)) => 400,
            BlazeDaemonError::Guest(crate::guest::GuestError::Timeout(_)) => 504,
            BlazeDaemonError::Guest(crate::guest::GuestError::OutcomeUnknown(_)) => 504,
            BlazeDaemonError::Guest(crate::guest::GuestError::PayloadTooLarge { .. }) => 413,
            BlazeDaemonError::Guest(crate::guest::GuestError::Cancelled) => 503,
            BlazeDaemonError::Guest(_) => 502,
            _ => 500,
        }
    }
}
