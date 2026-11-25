//! Bounded JSON decoding rejects duplicate keys before typed deserialization.

use std::collections::HashSet;
use std::fmt;

use serde::de::{self, DeserializeOwned, MapAccess, SeqAccess, Visitor};
use serde::{Deserialize, Deserializer};

use crate::protocol::{ContractError, MAX_MESSAGE_BYTES};

pub(crate) fn decode<T: DeserializeOwned>(bytes: &[u8]) -> Result<T, ContractError> {
    if bytes.len() > MAX_MESSAGE_BYTES {
        return Err(ContractError::MessageTooLarge);
    }
    // Keep serde_json's recursion limit and parse the original numeric tokens twice.
    serde_json::from_slice::<UniqueKeys>(bytes).map_err(|_| ContractError::InvalidJson)?;
    serde_json::from_slice(bytes).map_err(|_| ContractError::InvalidJson)
}

struct UniqueKeys;

impl<'de> Deserialize<'de> for UniqueKeys {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        deserializer.deserialize_any(UniqueVisitor)
    }
}

struct UniqueVisitor;

impl<'de> Visitor<'de> for UniqueVisitor {
    type Value = UniqueKeys;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("JSON with unique object keys")
    }

    fn visit_map<M: MapAccess<'de>>(self, mut map: M) -> Result<Self::Value, M::Error> {
        let mut keys = HashSet::new();
        while let Some(key) = map.next_key::<String>()? {
            if !keys.insert(key) {
                return Err(de::Error::custom("duplicate key"));
            }
            map.next_value::<UniqueKeys>()?;
        }
        Ok(UniqueKeys)
    }

    fn visit_seq<S: SeqAccess<'de>>(self, mut seq: S) -> Result<Self::Value, S::Error> {
        while seq.next_element::<UniqueKeys>()?.is_some() {}
        Ok(UniqueKeys)
    }

    fn visit_bool<E: de::Error>(self, _: bool) -> Result<Self::Value, E> {
        Ok(UniqueKeys)
    }

    fn visit_i64<E: de::Error>(self, _: i64) -> Result<Self::Value, E> {
        Ok(UniqueKeys)
    }

    fn visit_u64<E: de::Error>(self, _: u64) -> Result<Self::Value, E> {
        Ok(UniqueKeys)
    }

    fn visit_f64<E: de::Error>(self, _: f64) -> Result<Self::Value, E> {
        Ok(UniqueKeys)
    }

    fn visit_str<E: de::Error>(self, _: &str) -> Result<Self::Value, E> {
        Ok(UniqueKeys)
    }

    fn visit_unit<E: de::Error>(self) -> Result<Self::Value, E> {
        Ok(UniqueKeys)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;

    #[test]
    fn rejects_duplicates_at_every_depth() {
        for bytes in [
            br#"{"x":1,"x":2}"#.as_slice(),
            br#"{"metadata":[{"x":1,"\u0078":2}]}"#,
            br#"{"questions":{"preservation":{},"preservation":{}}}"#,
        ] {
            assert_eq!(decode::<Value>(bytes), Err(ContractError::InvalidJson));
        }
        assert!(decode::<Value>(br#"{"a":{"x":1},"b":{"x":2}}"#).is_ok());
    }

    #[test]
    fn keeps_integer_tokens_strict_and_limits_recursion() {
        assert!(decode::<u32>(b"1").is_ok());
        assert!(decode::<u32>(b"1.0").is_err());
        assert!(decode::<u32>(b"1e0").is_err());
        let deep = format!("{}0{}", "[".repeat(150), "]".repeat(150));
        assert_eq!(
            decode::<Value>(deep.as_bytes()),
            Err(ContractError::InvalidJson)
        );
        assert_eq!(
            decode::<Value>(&vec![b' '; MAX_MESSAGE_BYTES + 1]),
            Err(ContractError::MessageTooLarge)
        );
    }
}
