//! Parse into the JSON data model without losing duplicate keys or YAML types.

use crate::{Error, MAX_DEPTH, MAX_DOCUMENT_BYTES};
use serde::de::{self, DeserializeSeed, Deserializer, EnumAccess, MapAccess, SeqAccess, Visitor};
use serde_json::{Map, Number, Value};
use std::fmt;

#[derive(Default)]
struct Budget {
    expanded: usize,
    reason: Option<&'static str>,
}

impl Budget {
    fn fail<E: de::Error>(&mut self, reason: &'static str) -> E {
        self.reason = Some(reason);
        E::custom(reason)
    }

    fn charge<E: de::Error>(&mut self, bytes: usize) -> Result<(), E> {
        self.expanded = self.expanded.saturating_add(bytes);
        if self.expanded > MAX_DOCUMENT_BYTES {
            return Err(self.fail("expanded document exceeds size limit"));
        }
        Ok(())
    }
}

struct Checked<'a> {
    budget: &'a mut Budget,
    depth: usize,
    key: bool,
}

impl<'de> DeserializeSeed<'de> for Checked<'_> {
    type Value = Value;

    fn deserialize<D: Deserializer<'de>>(self, deserializer: D) -> Result<Value, D::Error> {
        if self.depth > MAX_DEPTH {
            return Err(self.budget.fail("document exceeds nesting limit"));
        }
        self.budget.charge(1)?;
        // deserialize_any preserves scalar types; deserialize_string would
        // silently coerce numeric/boolean YAML mapping keys into strings.
        deserializer.deserialize_any(self)
    }
}

impl Checked<'_> {
    fn value_only<E: de::Error>(&mut self) -> Result<(), E> {
        if self.key {
            return Err(self.budget.fail("mapping keys must be strings"));
        }
        Ok(())
    }
}

impl<'de> Visitor<'de> for Checked<'_> {
    type Value = Value;

    fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
        formatter.write_str("an unambiguous JSON-compatible YAML value")
    }

    fn visit_bool<E: de::Error>(mut self, value: bool) -> Result<Value, E> {
        self.value_only()?;
        Ok(Value::Bool(value))
    }

    fn visit_unit<E: de::Error>(mut self) -> Result<Value, E> {
        self.value_only()?;
        Ok(Value::Null)
    }

    fn visit_str<E: de::Error>(self, value: &str) -> Result<Value, E> {
        self.budget.charge(value.len())?;
        Ok(Value::String(value.to_owned()))
    }

    fn visit_i64<E: de::Error>(mut self, value: i64) -> Result<Value, E> {
        self.value_only()?;
        Ok(Value::Number(value.into()))
    }

    fn visit_u64<E: de::Error>(mut self, value: u64) -> Result<Value, E> {
        self.value_only()?;
        Ok(Value::Number(value.into()))
    }

    fn visit_f64<E: de::Error>(mut self, value: f64) -> Result<Value, E> {
        self.value_only()?;
        Number::from_f64(value)
            .map(Value::Number)
            .ok_or_else(|| self.budget.fail("non-finite numbers are not JSON values"))
    }

    fn visit_seq<A: SeqAccess<'de>>(mut self, mut seq: A) -> Result<Value, A::Error> {
        self.value_only()?;
        let mut values = Vec::new();
        while let Some(value) = seq.next_element_seed(Checked {
            budget: self.budget,
            depth: self.depth + 1,
            key: false,
        })? {
            values.push(value);
        }
        Ok(Value::Array(values))
    }

    fn visit_map<A: MapAccess<'de>>(mut self, mut map: A) -> Result<Value, A::Error> {
        self.value_only()?;
        let mut values = Map::new();
        while let Some(key) = map.next_key_seed(Checked {
            budget: self.budget,
            depth: self.depth + 1,
            key: true,
        })? {
            let Value::String(key) = key else {
                return Err(self.budget.fail("mapping keys must be strings"));
            };
            if key == "<<" {
                return Err(self.budget.fail("YAML merge keys are not supported"));
            }
            if values.contains_key(&key) {
                return Err(self.budget.fail("duplicate mapping key"));
            }
            let value = map.next_value_seed(Checked {
                budget: self.budget,
                depth: self.depth + 1,
                key: false,
            })?;
            values.insert(key, value);
        }
        Ok(Value::Object(values))
    }

    fn visit_enum<A: EnumAccess<'de>>(self, _: A) -> Result<Value, A::Error> {
        Err(self.budget.fail("custom YAML tags are not supported"))
    }
}

pub(super) fn parse(input: &[u8]) -> Result<Value, Error> {
    let invalid = |reason| Error::Document {
        reason,
        line: None,
        column: None,
    };
    if input.len() > MAX_DOCUMENT_BYTES {
        return Err(invalid("document exceeds size limit"));
    }
    let input = std::str::from_utf8(input).map_err(|_| invalid("expected UTF-8 input"))?;
    let mut budget = Budget::default();
    let mut documents = serde_yaml_ng::Deserializer::from_str(input);
    let document = documents
        .next()
        .ok_or_else(|| invalid("expected one document"))?;
    let value = Checked {
        budget: &mut budget,
        depth: 0,
        key: false,
    }
    .deserialize(document)
    .map_err(|error| Error::Document {
        reason: budget.reason.unwrap_or("invalid YAML syntax or scalar"),
        line: error.location().map(|location| location.line()),
        column: error.location().map(|location| location.column()),
    })?;
    if documents.next().is_some() {
        return Err(invalid("expected exactly one document"));
    }
    // Alias expansion has an incremental budget above. Also bound the exact
    // serialized size, including escaping, punctuation and numeric spellings.
    let encoded = serde_json::to_vec(&value).map_err(|_| invalid("invalid JSON value"))?;
    if encoded.len() > MAX_DOCUMENT_BYTES {
        return Err(invalid("expanded document exceeds size limit"));
    }
    Ok(value)
}
