#![forbid(unsafe_code)]
//! Offline external Provider messages and native Hook capability checks.
//!
//! This library neither runs commands nor authenticates an Adapter or Provider.
//! A successful check yields candidate effects, never a Core receipt or proof of
//! adoption. Trusted Hosts retain execution, budget and audit responsibilities.

pub mod admission;
mod parsing;
mod protocol;

pub use protocol::{Description, Outcome, Protocol, Reply, Request, ValidatedConfig};

/// Experimental external protocol, separate from Core's canonical wire records.
pub const VERSION: &str = "aw-provider/v1alpha1";
/// Maximum encoded request or response size; Hosts may impose a smaller limit.
pub const MAX_MESSAGE_BYTES: usize = 1024 * 1024;
/// Maximum JSON nesting depth, including opaque private and native values.
pub const MAX_DEPTH: usize = 32;
/// Bundled request schema for Provider implementers and offline tooling.
pub const REQUEST_SCHEMA: &str = include_str!("../schemas/request-v1alpha1.schema.json");
/// Bundled response schema; correlation and effect checks also require a request.
pub const RESPONSE_SCHEMA: &str = include_str!("../schemas/response-v1alpha1.schema.json");

/// Errors omit message bodies, configuration values and untrusted diagnostics.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// A message or capability relationship violates the protocol.
    #[error("invalid AW Provider contract: {0}")]
    Invalid(&'static str),
    /// Bundled schemas could not be compiled without external resources.
    #[error("invalid bundled AW Provider schema")]
    InvalidSchema,
    /// A valid, correlated Provider error; it is not a successful policy block.
    #[error("AW Provider reported a method failure")]
    ProviderFailure {
        /// Validated machine code, retained for caller-controlled diagnostics.
        code: String,
    },
}
