//! Compatible source encoding shared by PAP requests, CLI files and saved snapshots.

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::value::RawValue;

use super::PolicySource;

/// Serializes native policies in their historical shape and Cedar as a source envelope.
///
/// # Errors
/// Propagates serializer failures.
pub fn serialize<S: Serializer>(source: &PolicySource, serializer: S) -> Result<S::Ok, S::Error> {
    match source {
        PolicySource::SecCore(template) => template.serialize(serializer),
        PolicySource::Cedar(_) => source.serialize(serializer),
    }
}

/// Reads a source envelope or the historical native template without discarding fields.
///
/// # Errors
/// Rejects unknown languages, extra or duplicate fields and malformed source values.
pub fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<PolicySource, D::Error> {
    let raw = Box::<RawValue>::deserialize(deserializer)?;
    from_slice(raw.get().as_bytes()).map_err(|error| data_error(&error))
}

/// Reads a source envelope or native template, retaining original JSON error positions.
///
/// # Errors
/// Rejects unknown languages, extra or duplicate fields and malformed source values.
pub fn from_slice(bytes: &[u8]) -> Result<PolicySource, serde_json::Error> {
    // Inspect only the tag; typed parsing below uses the original bytes, including duplicates.
    let tagged = serde_json::from_slice::<std::collections::BTreeMap<String, &RawValue>>(bytes)
        .is_ok_and(|fields| fields.contains_key("language"));
    if tagged {
        serde_json::from_slice(bytes)
    } else {
        serde_json::from_slice(bytes).map(PolicySource::SecCore)
    }
}

fn data_error<E: serde::de::Error>(error: &serde_json::Error) -> E {
    let message = error.to_string();
    let location = format!(" at line {} column {}", error.line(), error.column());
    // Let the outer decoder attach its location, retaining historical native error text.
    E::custom(message.strip_suffix(&location).unwrap_or(&message))
}
