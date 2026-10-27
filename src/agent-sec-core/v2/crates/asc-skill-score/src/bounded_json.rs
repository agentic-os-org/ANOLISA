//! Bound untrusted JSON before allocating a DOM or growing a serialized output buffer.

use std::io::{self, Write};

use serde_json::Value;

use crate::{Error, Result};

pub(crate) struct JsonBudget(pub usize);

impl Default for JsonBudget {
    fn default() -> Self {
        Self(1_048_576)
    }
}

impl JsonBudget {
    /// Count an upper bound on DOM nodes, including object keys, without copying any input.
    pub fn parse(&mut self, input: &str) -> Result<Option<Value>> {
        let limit = self.0.min(262_144);
        let mut units = 1;
        if input.trim_start().starts_with(['{', '[']) {
            let (mut quoted, mut escaped) = (false, false);
            let mut depth = 0_usize;
            for byte in input.bytes() {
                if quoted {
                    if escaped {
                        escaped = false;
                    } else if byte == b'\\' {
                        escaped = true;
                    } else if byte == b'"' {
                        quoted = false;
                    }
                    continue;
                }
                match byte {
                    b'"' => quoted = true,
                    b'[' | b'{' => {
                        depth += 1;
                        units += 1;
                    }
                    b']' | b'}' => depth = depth.saturating_sub(1),
                    b',' | b':' => units += 1,
                    _ => continue,
                }
                if units > limit || depth > 96 {
                    return Err(Error::Runtime(
                        "evidence JSON exceeds structural limits".into(),
                    ));
                }
            }
        }
        self.0 = self.0.checked_sub(units).ok_or_else(|| {
            Error::Runtime("evidence JSON exceeds request structure budget".into())
        })?;
        Ok(serde_json::from_str(input).ok())
    }
}

pub(crate) struct JsonOutput {
    pub bytes: Vec<u8>,
    limit: usize,
}

impl JsonOutput {
    pub fn new(limit: usize) -> Self {
        Self {
            bytes: Vec::new(),
            limit,
        }
    }

    pub fn append(&mut self, value: &Value) -> Result<()> {
        Ok(serde_json::to_writer(self, value)?)
    }
}

impl Write for JsonOutput {
    fn write(&mut self, data: &[u8]) -> io::Result<usize> {
        if data.len() > self.limit - self.bytes.len() {
            return Err(io::Error::other(
                "serialized evidence exceeds its output budget",
            ));
        }
        let required = self.bytes.len() + data.len();
        if required > self.bytes.capacity() {
            let next = required
                .max(self.bytes.capacity().saturating_mul(2))
                .min(self.limit);
            self.bytes
                .try_reserve_exact(next - self.bytes.len())
                .map_err(io::Error::other)?;
        }
        self.bytes.extend_from_slice(data);
        Ok(data.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn delimiters_inside_escaped_strings_are_data_and_deep_structures_fail() -> Result<()> {
        let input = r#"{"braces":"\"[[[[[[,,,:::","next":"\\","end":0}"#;
        let mut budget = JsonBudget(7);
        assert_eq!(budget.parse(input)?.expect("valid JSON")["end"], 0);
        assert!(budget.parse("0").is_err());
        let deep = format!("{}0{}", "[".repeat(97), "]".repeat(97));
        assert!(JsonBudget::default().parse(&deep).is_err());
        Ok(())
    }
}
