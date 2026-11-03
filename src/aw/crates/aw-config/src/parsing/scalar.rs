//! Check float-producing YAML scalars against their original integer spelling.

use serde::de::{self, DeserializeSeed, Deserializer, IgnoredAny, MapAccess, SeqAccess, Visitor};
use serde::Deserialize;
use serde_json::Value;
use std::fmt;

/// Reject overflowing integer spellings at float-valued positions.
pub(super) fn check(input: &str, value: &Value) -> Result<(), serde_yaml_ng::Error> {
    // The first pass already bounded depth, alias expansion and document size.
    // Reuse its types so quoted/block strings never enter the integer check.
    Source(value).deserialize(serde_yaml_ng::Deserializer::from_str(input))
}

struct Source<'a>(&'a Value);

impl<'de> DeserializeSeed<'de> for Source<'_> {
    type Value = ();

    fn deserialize<D: Deserializer<'de>>(self, deserializer: D) -> Result<(), D::Error> {
        match self.0 {
            Value::Number(number) if number.is_f64() => deserializer.deserialize_str(Integer),
            Value::Array(_) | Value::Object(_) => deserializer.deserialize_any(self),
            _ => IgnoredAny::deserialize(deserializer).map(|_| ()),
        }
    }
}

impl<'de> Visitor<'de> for Source<'_> {
    type Value = ();

    fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
        formatter.write_str("the previously validated YAML structure")
    }

    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<(), A::Error> {
        if let Value::Array(values) = self.0 {
            for value in values {
                seq.next_element_seed(Source(value))?
                    .ok_or_else(|| de::Error::custom("missing validated sequence value"))?;
            }
        }
        Ok(())
    }

    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<(), A::Error> {
        while let Some(key) = map.next_key::<String>()? {
            let value = self
                .0
                .get(&key)
                .ok_or_else(|| de::Error::custom("missing validated mapping value"))?;
            map.next_value_seed(Source(value))?;
        }
        Ok(())
    }
}

struct Integer;

impl Visitor<'_> for Integer {
    type Value = ();

    fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
        formatter.write_str("a representable numeric scalar")
    }

    fn visit_str<E: de::Error>(self, token: &str) -> Result<(), E> {
        let digits = token
            .strip_prefix('+')
            .or_else(|| token.strip_prefix('-'))
            .unwrap_or(token);
        if !digits.is_empty()
            && digits.bytes().all(|byte| byte.is_ascii_digit())
            && token.parse::<i128>().is_err()
            && token.parse::<u128>().is_err()
        {
            return Err(E::custom("integer is outside the supported range"));
        }
        Ok(())
    }
}
