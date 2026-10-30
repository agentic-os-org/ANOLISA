//! Integer-only canonical metadata encoding with exact UTF-8 content strings.
//!
//! This is AW JSON v1, not RFC 8785. Restricting metadata keys to ASCII and
//! numbers to safe integers makes the same representation portable to JS,
//! Python and Rust without changing arbitrary numbers inside content strings.

use serde::de::{self, Deserialize, Deserializer, MapAccess, SeqAccess, Visitor};
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};
use std::fmt;

use crate::Error;

/// Maximum encoded document size, checked before parsing untrusted input.
pub const MAX_DOCUMENT_BYTES: usize = 4 * 1024 * 1024;
/// Largest exactly representable integer admitted to AW metadata.
pub const MAX_SAFE_INTEGER: u64 = 9_007_199_254_740_991;

/// Parses JSON without losing duplicate keys or accepting float spellings.
///
/// # Errors
/// Rejects oversized, ambiguous or noncanonical-domain input. Object ordering
/// and insignificant whitespace are accepted and normalized by [`bytes`].
pub fn parse(input: &[u8]) -> Result<Value, Error> {
    if input.len() > MAX_DOCUMENT_BYTES {
        return Err(Error::InvalidDocument);
    }
    let mut parser = serde_json::Deserializer::from_slice(input);
    let parsed = Checked::deserialize(&mut parser).map_err(|_| Error::InvalidDocument)?;
    parser.end().map_err(|_| Error::InvalidDocument)?;
    bytes(&parsed.0)?;
    Ok(parsed.0)
}

struct Checked(Value);

impl<'de> Deserialize<'de> for Checked {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct CheckedVisitor;
        impl<'de> Visitor<'de> for CheckedVisitor {
            type Value = Checked;
            fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
                formatter.write_str("an unambiguous AW JSON value")
            }
            fn visit_bool<E: de::Error>(self, value: bool) -> Result<Checked, E> {
                Ok(Checked(Value::Bool(value)))
            }
            fn visit_unit<E: de::Error>(self) -> Result<Checked, E> {
                Ok(Checked(Value::Null))
            }
            fn visit_str<E: de::Error>(self, value: &str) -> Result<Checked, E> {
                Ok(Checked(Value::String(value.to_owned())))
            }
            fn visit_i64<E: de::Error>(self, value: i64) -> Result<Checked, E> {
                if value.unsigned_abs() > MAX_SAFE_INTEGER {
                    return Err(E::custom("unsafe integer"));
                }
                Ok(Checked(value.into()))
            }
            fn visit_u64<E: de::Error>(self, value: u64) -> Result<Checked, E> {
                if value > MAX_SAFE_INTEGER {
                    return Err(E::custom("unsafe integer"));
                }
                Ok(Checked(value.into()))
            }
            fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Checked, A::Error> {
                let mut values = Vec::new();
                while let Some(value) = seq.next_element::<Checked>()? {
                    values.push(value.0);
                }
                Ok(Checked(Value::Array(values)))
            }
            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Checked, A::Error> {
                let mut values = Map::new();
                while let Some(key) = map.next_key::<String>()? {
                    if !key.is_ascii() || values.contains_key(&key) {
                        return Err(de::Error::custom("invalid or duplicate metadata key"));
                    }
                    values.insert(key, map.next_value::<Checked>()?.0);
                }
                Ok(Checked(Value::Object(values)))
            }
        }
        deserializer.deserialize_any(CheckedVisitor)
    }
}

/// Sorts ASCII object keys, preserves arrays and emits compact UTF-8 JSON.
///
/// # Errors
/// Rejects depth over 32, non-ASCII keys, floating point or unsafe integers,
/// and encoded documents over [`MAX_DOCUMENT_BYTES`].
pub fn bytes(value: &Value) -> Result<Vec<u8>, Error> {
    fn normalize(value: &Value, depth: usize) -> Result<Value, Error> {
        if depth > 32 {
            return Err(Error::InvalidDocument);
        }
        match value {
            Value::Object(map) => {
                let mut pairs = map.iter().collect::<Vec<_>>();
                pairs.sort_by_key(|(key, _)| *key);
                let mut result = Map::new();
                for (key, value) in pairs {
                    if !key.is_ascii() {
                        return Err(Error::InvalidDocument);
                    }
                    result.insert(key.clone(), normalize(value, depth + 1)?);
                }
                Ok(Value::Object(result))
            }
            Value::Array(array) => array
                .iter()
                .map(|value| normalize(value, depth + 1))
                .collect::<Result<Vec<_>, _>>()
                .map(Value::Array),
            Value::Number(number) => {
                let valid = number.as_u64().is_some_and(|v| v <= MAX_SAFE_INTEGER)
                    || number
                        .as_i64()
                        .is_some_and(|v| v.unsigned_abs() <= MAX_SAFE_INTEGER);
                if !valid {
                    return Err(Error::InvalidDocument);
                }
                Ok(value.clone())
            }
            _ => Ok(value.clone()),
        }
    }
    let encoded = serde_json::to_vec(&normalize(value, 0)?).map_err(|_| Error::InvalidDocument)?;
    if encoded.len() > MAX_DOCUMENT_BYTES {
        return Err(Error::InvalidDocument);
    }
    Ok(encoded)
}

/// Computes lowercase SHA-256 of exact bytes without text normalization.
pub fn digest(input: &[u8]) -> String {
    format!("{:x}", Sha256::digest(input))
}

/// Computes the digest of an AW canonical metadata document.
///
/// # Errors
/// Returns the encoding failures described in [`bytes`].
pub fn document_digest(value: &Value) -> Result<String, Error> {
    Ok(digest(&bytes(value)?))
}

#[cfg(test)]
mod tests {
    use super::{digest, document_digest, parse};

    #[test]
    fn document_normalization_does_not_redefine_raw_byte_identity() {
        let left = br#"{ "b": 2, "a": 1 }"#;
        let right = br#"{"a":1,"b":2}"#;
        assert_ne!(digest(left), digest(right));
        assert_eq!(
            document_digest(&parse(left).unwrap()).unwrap(),
            document_digest(&parse(right).unwrap()).unwrap()
        );
    }
}
