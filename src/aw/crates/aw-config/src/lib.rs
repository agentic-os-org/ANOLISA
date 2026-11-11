#![forbid(unsafe_code)]
//! Side-effect-free parsing of desired AW configuration, separate from wire records.
//!
//! A valid document is not an installed binding or a capability grant. Provider
//! discovery, private configuration validation and native effect admission belong
//! to the service and adapter layers; this crate never starts those components.

pub mod native;

mod parsing;
mod policy_update;
mod validation;

use serde_json::Value;
use std::sync::OnceLock;

/// Authoritative configuration shape, available to offline editor tooling.
pub const SCHEMA: &str = include_str!("../schemas/configuration-v1alpha1.schema.json");
/// Default command contract: one normalized event in, one JSON boolean out.
pub const HOOK_PROTOCOL: &str = "aw-hook/v1alpha1";
/// Maximum input and expanded JSON size in bytes.
pub const MAX_DOCUMENT_BYTES: usize = 4 * 1024 * 1024;
/// Maximum nesting depth, including Provider-owned configuration.
pub const MAX_DEPTH: usize = 32;

/// Parses one bounded YAML or JSON value without checking configuration shape.
///
/// Use this for editor definitions, then validate the complete edited document
/// with [`Validator`]. Mapping keys remain strings and scalar types are retained.
///
/// # Errors
/// Rejects duplicate keys, ambiguous YAML constructs, non-JSON scalars, multiple
/// documents and input exceeding the byte, expansion or nesting limits.
/// Diagnostics omit the supplied values.
pub fn parse_value(input: &[u8]) -> Result<Value, Error> {
    parsing::parse(input)
}

/// Errors report locations and constraints without printing configuration values.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// Invalid, ambiguous, oversized or excessively nested YAML/JSON.
    #[error("invalid AW configuration: {reason} (line {line:?}, column {column:?})")]
    Document {
        /// Constraint that failed, without source text.
        reason: &'static str,
        /// One-based source line when the parser provides a location.
        line: Option<usize>,
        /// One-based source column when the parser provides a location.
        column: Option<usize>,
    },
    /// The schema bundled with this build could not be compiled.
    #[error("invalid bundled AW configuration schema")]
    InvalidSchema,
    /// A field violates the public shape; both locations are JSON Pointers.
    #[error("AW configuration field {path}: {reason} ({constraint})")]
    Shape {
        /// Location in the supplied document.
        path: String,
        /// Human-readable constraint with instance values masked.
        reason: String,
        /// Location of the failed schema keyword.
        constraint: String,
    },
    /// A structurally valid field violates a cross-field rule.
    #[error("AW configuration field {path}: {reason}")]
    Invariant {
        /// Location in the supplied document.
        path: String,
        /// Failed constraint without configuration values.
        reason: &'static str,
    },
}

/// Desired configuration that passed syntax, shape and static reference checks.
///
/// Omitted defaults remain omitted. This document contains no runtime status,
/// discovered capabilities or proof of successful Provider or Agent execution.
pub struct Configuration(Value);

impl Configuration {
    /// Borrows the validated JSON-compatible document without invoking providers.
    pub fn as_value(&self) -> &Value {
        &self.0
    }
}

/// Reusable offline validator compiled from the bundled configuration schema.
pub struct Validator(&'static jsonschema::Validator);

impl Validator {
    /// Reuses the process-wide schema without fetching network resources.
    ///
    /// # Errors
    /// Returns [`Error::InvalidSchema`] if the bundled schema is invalid.
    pub fn new() -> Result<Self, Error> {
        static SCHEMA_VALIDATOR: OnceLock<Result<jsonschema::Validator, ()>> = OnceLock::new();
        SCHEMA_VALIDATOR
            .get_or_init(|| {
                let schema: Value = serde_json::from_str(SCHEMA).map_err(|_| ())?;
                jsonschema::draft202012::options()
                    .offline()
                    .build(&schema)
                    .map_err(|_| ())
            })
            .as_ref()
            .map(Self)
            .map_err(|_| Error::InvalidSchema)
    }

    /// Parses one bounded YAML (or JSON) document and checks static invariants.
    ///
    /// # Errors
    /// Rejects duplicate keys, invalid public fields, unresolved Provider/guard
    /// references and inconsistent event actions. Provider-owned `config` stays
    /// opaque JSON; its schema and requested capabilities require later admission.
    pub fn parse(&self, input: &[u8]) -> Result<Configuration, Error> {
        let document = parse_value(input)?;
        self.0.validate(&document).map_err(|error| Error::Shape {
            path: error.instance_path().to_string(),
            reason: error.masked().to_string(),
            constraint: error.schema_path().to_string(),
        })?;
        validation::validate(&document)?;
        native::validate(&document)?;
        Ok(Configuration(document))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shared_schema_keeps_document_validation_independent() {
        let first = Validator::new().unwrap();
        let second = Validator::new().unwrap();
        assert!(std::ptr::eq(first.0, second.0));
        let example = include_bytes!("../examples/aw.minimal.yaml");
        assert!(first.parse(example).is_ok());
        assert!(second.parse(b"{}").is_err());
        assert!(second.parse(example).is_ok());
    }
}
