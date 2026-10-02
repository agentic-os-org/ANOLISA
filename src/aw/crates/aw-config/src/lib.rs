#![forbid(unsafe_code)]
//! Side-effect-free parsing of desired AW configuration, separate from wire records.
//!
//! A valid document is not an installed binding or a capability grant. Provider
//! discovery, private configuration validation and native effect admission belong
//! to the service and adapter layers; this crate never starts those components.

mod parsing;
mod validation;

use serde_json::Value;

/// Authoritative configuration shape, available to offline editor tooling.
pub const SCHEMA: &str = include_str!("../schemas/configuration-v1alpha1.schema.json");
/// Maximum input and expanded JSON size in bytes.
pub const MAX_DOCUMENT_BYTES: usize = 4 * 1024 * 1024;
/// Maximum nesting depth, including Provider-owned configuration.
pub const MAX_DEPTH: usize = 32;

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
pub struct Validator(jsonschema::Validator);

impl Validator {
    /// Compiles the schema without fetching network resources.
    ///
    /// # Errors
    /// Returns [`Error::InvalidSchema`] if the bundled schema is invalid.
    pub fn new() -> Result<Self, Error> {
        let schema: Value = serde_json::from_str(SCHEMA).map_err(|_| Error::InvalidSchema)?;
        jsonschema::draft202012::options()
            .offline()
            .build(&schema)
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
        let document = parsing::parse(input)?;
        self.0.validate(&document).map_err(|error| Error::Shape {
            path: error.instance_path().to_string(),
            reason: error.masked().to_string(),
            constraint: error.schema_path().to_string(),
        })?;
        validation::validate(&document)?;
        Ok(Configuration(document))
    }
}
