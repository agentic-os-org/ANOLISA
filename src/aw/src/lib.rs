#![forbid(unsafe_code)]
//! AW capability schemas and side-effect-free boundary validation.
//!
//! Callers authenticate authorities, capture native boundaries and persist
//! records. Validation establishes consistency of supplied evidence, never OS
//! ownership, scanner correctness, durable storage or remote model consumption.

pub mod canonical;
pub mod orchestration;
pub mod registry;
pub mod validation;

pub use registry::Registry;

/// Contract failures deliberately omit potentially sensitive payload values.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// Input is not a bounded, unambiguous AW JSON document.
    #[error("invalid AW JSON document")]
    InvalidDocument,
    /// A bundled schema could not be compiled.
    #[error("invalid bundled schema: {0}")]
    InvalidSchema(String),
    /// The exact schema revision is not registered.
    #[error("unsupported schema: {0}")]
    UnsupportedSchema(String),
    /// The value does not satisfy its declared schema.
    #[error("schema mismatch: {0}")]
    SchemaMismatch(String),
    /// Two otherwise well-shaped records violate a semantic invariant.
    #[error("contract invariant failed: {0}")]
    Invariant(&'static str),
}

pub(crate) fn require(condition: bool, invariant: &'static str) -> Result<(), Error> {
    if condition {
        Ok(())
    } else {
        Err(Error::Invariant(invariant))
    }
}
