//! JSON fields keep their length contract and a trailing NUL in one allocation.

use serde::Serialize;
use std::ffi::c_char;

pub(super) struct JsonBuffer(Vec<u8>);

impl JsonBuffer {
    pub(super) fn new(value: &impl Serialize) -> Self {
        let mut bytes = Vec::new();
        if let Err(error) = serde_json::to_writer(&mut bytes, value) {
            log::warn!("Failed to serialize FFI JSON field: {error}");
            bytes.clear();
        }
        // JSON escapes interior NUL bytes; no replacement or CString copy is needed.
        bytes.push(0);
        Self(bytes)
    }

    pub(super) fn as_ptr(&self) -> *const c_char {
        self.0.as_ptr().cast()
    }
    pub(super) fn len(&self) -> usize {
        self.0.len() - 1
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn json_escapes_nul_and_preserves_utf8_without_changing_field_length() {
        let value = serde_json::json!(["hello\u{0}世界"]);
        let bytes = JsonBuffer::new(&value);
        let expected = serde_json::to_vec(&value).expect("JSON serializes");
        assert_eq!(bytes.len(), expected.len());
        assert_eq!(&bytes.0[..bytes.len()], expected);
        assert_eq!(bytes.0[bytes.len()], 0);
        assert!(!bytes.0[..bytes.len()].contains(&0));
    }
}
