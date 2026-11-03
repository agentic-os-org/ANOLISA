//! Retained JSON allocation estimates without allocating a serialized copy.

use serde_json::Value;
use std::mem::size_of;

pub(crate) fn json_bytes(value: &Value) -> usize {
    match value {
        Value::String(value) => value.capacity(),
        Value::Array(values) => {
            values.capacity() * size_of::<Value>() + values.iter().map(json_bytes).sum::<usize>()
        }
        Value::Object(values) => {
            // Include key/value slots as well as owned strings; allocator metadata
            // remains covered by the process's memory headroom.
            values.len() * size_of::<(String, Value)>()
                + values
                    .iter()
                    .map(|(key, value)| key.capacity() + json_bytes(value))
                    .sum::<usize>()
        }
        _ => 0,
    }
}
