//! Compatible source encoding shared by PAP requests, CLI files and saved snapshots.

use serde::{Deserialize, Deserializer, Serialize, Serializer};

use serde_json::value::RawValue;

use super::{PolicySource, PolicyTemplate};

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

/// Accepts a native template or a tagged source without accepting mixed fields.
///
/// # Errors
/// Rejects malformed, duplicate, or unknown source fields.
pub fn deserialize<'de, D>(deserializer: D) -> Result<PolicySource, D::Error>
where
    D: Deserializer<'de>,
{
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Input {
        Native(PolicyTemplate),
        Source(PolicySource),
        Invalid(serde_json::Value),
    }
    let value = match Input::deserialize(deserializer)? {
        Input::Native(template) => return Ok(PolicySource::SecCore(template)),
        Input::Source(source) => return Ok(source),
        Input::Invalid(value) => value,
    };
    // Value is diagnostic-only: it may have collapsed duplicate fields rejected above.
    let error = if value.get("language").is_some() {
        serde_json::from_value::<PolicySource>(value).err()
    } else {
        serde_json::from_value::<PolicyTemplate>(value).err()
    };
    Err(serde::de::Error::custom(error.map_or_else(
        || "malformed policy source".to_owned(),
        |error| error.to_string(),
    )))
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
