//! TOON views that the bundled decoder can read back.
//!
//! `toon-format` renders an array whose elements are all empty objects as a
//! tabular block with an empty field list — `{"items":[{},{}]}` encodes to
//! `items[2]{}:` — and its own decoder rejects that shape with "Field list
//! cannot be empty for tabular arrays". Selecting such a view would replace a
//! tool result with text that cannot be turned back into the original payload,
//! and the TOON paths write no Stash entry, so the loss would be permanent.
//! Every TOON candidate therefore has to survive a decode before it can be
//! offered to the model.

/// Returns true when `candidate` parses back into a JSON value.
///
/// Only decodability is checked, not equality with the encoded value:
/// `toon-format` prints some numbers in a form that decodes to a different
/// JSON type (`1.0` comes back as `1`), and refusing those would drop TOON
/// coverage for ordinary payloads without making a single view more readable.
#[must_use]
pub fn toon_decodes(candidate: &str) -> bool {
    toon_format::decode_default::<serde_json::Value>(candidate).is_ok()
}

#[cfg(test)]
mod tests {
    use super::toon_decodes;
    use serde_json::json;

    fn encoded(value: serde_json::Value) -> String {
        toon_format::encode_default(&value)
            .unwrap()
            .trim_end()
            .to_owned()
    }

    #[test]
    fn rejects_the_tabular_form_of_an_all_empty_object_array() {
        // The encoder's tabular header carries no field names here, which is
        // exactly what the decoder refuses to read.
        for value in [
            json!({"items": [{}, {}]}),
            json!({"items": [{}]}),
            json!([{}, {}]),
            json!({"a": {"b": [{}, {}, {}]}}),
            json!({"rows": [{"x": 1}, {}], "other": [{}]}),
            json!({"a": [[{}], [{}]]}),
        ] {
            let candidate = encoded(value.clone());
            assert!(candidate.contains("{}:"), "{candidate}");
            assert!(!toon_decodes(&candidate), "{candidate}");
        }
    }

    #[test]
    fn accepts_every_other_encoding_the_encoder_produces() {
        for value in [
            json!({"items": [{"id": 1}, {"id": 2}]}),
            json!({"empty_array": [], "empty_object": {}}),
            json!({"a": [1, 2, 3], "b": "text", "c": null}),
            json!([1, 2, 3]),
            json!([]),
            json!("text"),
            json!(null),
        ] {
            let candidate = encoded(value);
            assert!(toon_decodes(&candidate), "{candidate}");
        }
    }
}
