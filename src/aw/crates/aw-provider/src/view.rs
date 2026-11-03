//! Independent, read-only presentation messages; never event effects or permissions.

use crate::{parsing, Error};
use serde_json::Value;

/// Optional presentation protocol, separate from the event Provider protocol.
pub const VERSION: &str = "aw-provider-view/v1alpha1";
/// Panel snapshot version consumed by desktop clients.
pub const PANEL_VERSION: &str = "aw-panel/v1alpha1";
/// Offline schemas; `$defs/panel` describes the complete inspect result.
pub const SCHEMA: &str = include_str!("../schemas/view-v1alpha1.schema.json");

/// Decode bounded JSON, preserving duplicate-key, integer and nesting checks.
///
/// # Errors
/// Rejects ambiguous, malformed or oversized external JSON.
pub fn decode(bytes: &[u8]) -> Result<Value, Error> {
    parsing::parse(bytes)
}

/// Current Unix milliseconds, suitable for presentation observation timestamps.
///
/// # Errors
/// Rejects clocks before the epoch or outside JSON's interoperable integer range.
pub fn now_ms() -> Result<u64, Error> {
    let value = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|_| Error::Invalid("clock before epoch"))?
        .as_millis();
    if value > 9_007_199_254_740_991 {
        return Err(Error::Invalid("clock outside presentation range"));
    }
    Ok(value as u64)
}

/// Compiled request, response and Panel schemas with strict correlation checks.
pub struct Protocol {
    request: jsonschema::Validator,
    response: jsonschema::Validator,
    panel: jsonschema::Validator,
}

impl Protocol {
    /// Compile bundled schemas without network access.
    ///
    /// # Errors
    /// Returns `InvalidSchema` for an invalid bundled schema.
    pub fn new() -> Result<Self, Error> {
        let compile = |section: &str| {
            let mut schema: Value =
                serde_json::from_str(SCHEMA).map_err(|_| Error::InvalidSchema)?;
            schema["$ref"] = Value::String(format!("#/$defs/{section}"));
            jsonschema::draft202012::options()
                .offline()
                .build(&schema)
                .map_err(|_| Error::InvalidSchema)
        };
        Ok(Self {
            request: compile("request")?,
            response: compile("response")?,
            panel: compile("panel")?,
        })
    }

    /// Parse one bounded request without accepting duplicate keys or extra fields.
    ///
    /// # Errors
    /// Rejects invalid versions, schemas, budgets, identities or JSON.
    pub fn parse_request(&self, bytes: &[u8]) -> Result<Value, Error> {
        let value = parsing::parse(bytes)?;
        if !self.request.is_valid(&value) {
            return Err(Error::Invalid("view request schema mismatch"));
        }
        Ok(value)
    }

    /// Check a response against its exact request; no Provider data grants effects.
    ///
    /// # Errors
    /// Rejects invalid JSON, schemas, versions and correlation mismatches.
    pub fn check_response(&self, request: &Value, bytes: &[u8]) -> Result<Value, Error> {
        let value = parsing::parse(bytes)?;
        if !self.request.is_valid(request) || !self.response.is_valid(&value) {
            return Err(Error::Invalid("view response schema mismatch"));
        }
        for key in [
            "protocol",
            "operation",
            "request_id",
            "provider_id",
            "config_revision",
        ] {
            if value[key] != request[key] {
                return Err(Error::Invalid("view response identity mismatch"));
            }
        }
        Ok(value)
    }

    /// Validate a complete, bounded Panel snapshot without retaining private data.
    ///
    /// # Errors
    /// Rejects invalid JSON, unknown fields and inconsistent data shapes.
    pub fn check_panel(&self, bytes: &[u8]) -> Result<Value, Error> {
        let value = parsing::parse(bytes)?;
        if !self.panel.is_valid(&value) {
            return Err(Error::Invalid("panel snapshot schema mismatch"));
        }
        Ok(value)
    }
}
