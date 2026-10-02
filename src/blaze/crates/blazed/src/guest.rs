// SPDX-License-Identifier: Apache-2.0
//! Firecracker-vsock guest agent client.

pub mod client;

pub use client::GuestClient;
pub use client::GuestExecResult;

use thiserror::Error;

/// Maximum decoded file payload accepted by guest read and write operations.
pub(crate) const MAX_GUEST_FILE_BYTES: usize = 16 * 1024 * 1024;

/// Guest protocol and transport failures.
#[derive(Debug, Error)]
pub enum GuestError {
    /// Unix socket I/O failed.
    #[error("guest transport error: {0}")]
    Io(#[from] std::io::Error),
    /// JSON encoding or decoding failed.
    #[error("guest JSON error: {0}")]
    Json(#[from] serde_json::Error),
    /// Firecracker vsock or guest framing was invalid.
    #[error("guest protocol error: {0}")]
    Protocol(String),
    /// Caller supplied an invalid guest operation argument.
    #[error("invalid guest request: {0}")]
    InvalidArgument(String),
    /// A bounded guest operation timed out.
    #[error("guest operation timed out: {0}")]
    Timeout(String),
    /// A state-changing request timed out after it may have reached the guest.
    #[error("guest operation outcome is unknown: {0}")]
    OutcomeUnknown(String),
    /// The guest returned an application error.
    #[error("guest operation failed: {0}")]
    Rejected(String),
    /// Caller-supplied decoded file data exceeded the guest file hard limit.
    #[error("guest payload too large: {actual} bytes exceeds {limit}")]
    PayloadTooLarge {
        /// Decoded or framed byte count.
        actual: usize,
        /// Configured limit.
        limit: usize,
    },
    /// A guest response exceeded the bounded frame or decoded output limit.
    #[error("guest response too large: {actual} bytes exceeds {limit}")]
    ResponseTooLarge {
        /// Decoded or framed byte count.
        actual: usize,
        /// Configured limit.
        limit: usize,
    },
    /// Readiness polling was cancelled by its caller.
    #[error("guest readiness wait cancelled")]
    Cancelled,
}

/// Result alias for guest operations.
pub type Result<T> = std::result::Result<T, GuestError>;
