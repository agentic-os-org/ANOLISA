//! Bound JSON before schema validation without collapsing duplicate keys.

use crate::{Error, MAX_DEPTH, MAX_MESSAGE_BYTES};
use serde::de::{self, DeserializeSeed, Deserializer, MapAccess, SeqAccess, Visitor};
use serde_json::{Map, Number, Value};
use std::fmt;

struct Checked(usize);

impl<'de> DeserializeSeed<'de> for Checked {
    type Value = Value;

    fn deserialize<D: Deserializer<'de>>(self, decoder: D) -> Result<Value, D::Error> {
        if self.0 > MAX_DEPTH {
            return Err(de::Error::custom("nesting limit"));
        }
        decoder.deserialize_any(self)
    }
}

impl<'de> Visitor<'de> for Checked {
    type Value = Value;

    fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
        formatter.write_str("bounded, unambiguous JSON")
    }

    fn visit_bool<E: de::Error>(self, value: bool) -> Result<Value, E> {
        Ok(Value::Bool(value))
    }

    fn visit_unit<E: de::Error>(self) -> Result<Value, E> {
        Ok(Value::Null)
    }

    fn visit_str<E: de::Error>(self, value: &str) -> Result<Value, E> {
        Ok(Value::String(value.into()))
    }

    fn visit_i64<E: de::Error>(self, value: i64) -> Result<Value, E> {
        Ok(Value::Number(value.into()))
    }

    fn visit_u64<E: de::Error>(self, value: u64) -> Result<Value, E> {
        Ok(Value::Number(value.into()))
    }

    fn visit_f64<E: de::Error>(self, value: f64) -> Result<Value, E> {
        Number::from_f64(value)
            .map(Value::Number)
            .ok_or_else(|| de::Error::custom("non-finite number"))
    }

    fn visit_seq<A: SeqAccess<'de>>(self, mut sequence: A) -> Result<Value, A::Error> {
        let mut values = Vec::new();
        while let Some(value) = sequence.next_element_seed(Checked(self.0 + 1))? {
            values.push(value);
        }
        Ok(Value::Array(values))
    }

    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Value, A::Error> {
        let mut values = Map::new();
        while let Some(key) = map.next_key::<String>()? {
            if values.contains_key(&key) {
                return Err(de::Error::custom("duplicate key"));
            }
            values.insert(key, map.next_value_seed(Checked(self.0 + 1))?);
        }
        Ok(Value::Object(values))
    }
}

pub(crate) fn parse(bytes: &[u8]) -> Result<Value, Error> {
    if bytes.len() > MAX_MESSAGE_BYTES {
        return Err(Error::Invalid("message size limit"));
    }
    check_integer_range(bytes)?;
    let mut decoder = serde_json::Deserializer::from_slice(bytes);
    let value = Checked(0)
        .deserialize(&mut decoder)
        .map_err(|_| Error::Invalid("expected one bounded, unambiguous JSON value"))?;
    decoder
        .end()
        .map_err(|_| Error::Invalid("unexpected trailing data"))?;
    Ok(value)
}

fn check_integer_range(bytes: &[u8]) -> Result<(), Error> {
    // serde_json otherwise turns overflowing integer tokens into rounded f64s.
    // Inspect only unquoted tokens; strings containing large IDs remain opaque.
    let mut index = 0;
    while index < bytes.len() {
        match bytes[index] {
            b'"' => {
                index += 1;
                while index < bytes.len() && bytes[index] != b'"' {
                    index += if bytes[index] == b'\\' { 2 } else { 1 };
                }
                index += 1;
            }
            b'-' | b'0'..=b'9' => {
                let start = index;
                while index < bytes.len()
                    && matches!(bytes[index], b'0'..=b'9' | b'-' | b'+' | b'.' | b'e' | b'E')
                {
                    index += 1;
                }
                let token = &bytes[start..index];
                if !token.iter().any(|b| matches!(b, b'.' | b'e' | b'E')) {
                    let text = std::str::from_utf8(token)
                        .map_err(|_| Error::Invalid("invalid numeric token"))?;
                    let fits = if text.starts_with('-') {
                        text.parse::<i64>().is_ok()
                    } else {
                        text.parse::<u64>().is_ok()
                    };
                    if !fits {
                        return Err(Error::Invalid("integer is outside the supported range"));
                    }
                }
            }
            _ => index += 1,
        }
    }
    Ok(())
}
