//! Decode Qoder CLI requests and adapt the gateway's chat payloads.

use base64::{Engine, engine::general_purpose::STANDARD};
use serde_json::{Value, json};

use super::{OpenAIParser, OpenAIRequest, OpenAIResponse};

const ALPHABET: &[u8; 64] = b"_doRTgHZBKcGVjlvpC,@aFSx#DPuNJme&i*MzLOEn)sUrthbf%Y^w.(kIQyXqWA!";
const STANDARD_ALPHABET: &[u8; 64] =
    b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

/// Wire byte -> standard Base64 byte, built once instead of scanning the
/// alphabet per input byte (request bodies reach hundreds of KiB).
static TRANSLATION: std::sync::LazyLock<[Option<u8>; 256]> = std::sync::LazyLock::new(|| {
    let mut table = [None; 256];
    for (index, &wire) in ALPHABET.iter().enumerate() {
        table[wire as usize] = Some(STANDARD_ALPHABET[index]);
    }
    table[b'$' as usize] = Some(b'=');
    table
});

fn qoder_decode(encoded: &str) -> Option<Vec<u8>> {
    let encoded = encoded.trim().as_bytes();
    if encoded.is_empty() || !encoded.len().is_multiple_of(4) {
        return None;
    }
    // The wire format swaps the outer thirds and substitutes the Base64 alphabet.
    let third = encoded.len() / 3;
    let reordered = encoded[encoded.len() - third..]
        .iter()
        .chain(&encoded[third..encoded.len() - third])
        .chain(&encoded[..third]);
    let translated: Option<Vec<u8>> = reordered.map(|byte| TRANSLATION[*byte as usize]).collect();
    STANDARD.decode(translated?).ok()
}

/// Adapts the encoded gateway protocol without adding another capture probe.
pub(crate) struct QoderCliParser;

impl QoderCliParser {
    /// Restrict normalization to the chat generation endpoint.
    pub(crate) fn matches_path(path: &str) -> bool {
        crate::parser::llm::is_qoder_path(path)
    }

    /// Reject malformed encoding or non-JSON payloads without fabricating a request.
    pub(crate) fn decode_body_bytes(body: &[u8]) -> Option<Value> {
        let decoded = qoder_decode(std::str::from_utf8(body).ok()?)?;
        serde_json::from_slice(&decoded).ok()
    }

    /// Preserve the gateway's separate system prompt and generation parameters.
    pub(crate) fn parse_request(body: &Value) -> Option<OpenAIRequest> {
        let mut body = body.clone();
        let object = body.as_object_mut()?;
        let model = object
            .get("model_config")
            .and_then(|config| {
                config
                    .get("model")
                    .and_then(Value::as_str)
                    .filter(|s| !s.is_empty())
                    .or_else(|| {
                        config
                            .get("key")
                            .and_then(Value::as_str)
                            .filter(|s| !s.is_empty())
                    })
            })
            .unwrap_or("qodercli")
            .to_owned();
        object.insert("model".into(), json!(model));
        for field in ["max_tokens", "temperature", "top_p"] {
            if let Some(value) = object
                .get("parameters")
                .and_then(|parameters| parameters.get(field))
                .cloned()
            {
                object.insert(field.into(), value);
            }
        }
        if let Some(system) = object
            .get("system")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .map(str::to_owned)
        {
            let messages = object
                .entry("messages")
                .or_insert_with(|| json!([]))
                .as_array_mut()?;
            messages.insert(0, json!({"role": "system", "content": system}));
        }
        normalize_arguments(&mut body);
        OpenAIParser::parse_request(&body)
    }

    /// Normalize complete argument objects before reusing OpenAI stream assembly.
    pub(crate) fn parse_response(body: &Value) -> Option<OpenAIResponse> {
        let mut body = body.clone();
        if let Some(chunks) = body.as_array_mut() {
            for chunk in chunks {
                *chunk = crate::parser::llm::unwrap_qoder_envelope(chunk)
                    .unwrap_or_else(|| chunk.take());
                normalize_arguments(chunk);
            }
        } else {
            body = crate::parser::llm::unwrap_qoder_envelope(&body).unwrap_or(body);
            normalize_arguments(&mut body);
        }
        OpenAIParser::parse_response(&body)
    }
}

fn normalize_arguments(body: &mut Value) {
    // Qoder may send a complete arguments object where OpenAI expects JSON text.
    for field in ["messages", "choices"] {
        if let Some(messages) = body.get_mut(field).and_then(Value::as_array_mut) {
            for message in messages {
                for pointer in ["/tool_calls", "/delta/tool_calls", "/message/tool_calls"] {
                    if let Some(calls) = message.pointer_mut(pointer).and_then(Value::as_array_mut)
                    {
                        for call in calls {
                            if let Some(arguments) = call
                                .pointer_mut("/function/arguments")
                                .filter(|value| value.is_object())
                            {
                                *arguments = Value::String(arguments.to_string());
                            }
                        }
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decode_fixed_vectors_and_reject_malformed_input() {
        // Fixed vectors also exercise padding at its rotated wire position.
        assert_eq!(qoder_decode("$&$D"), Some(b"f".to_vec()));
        for encoded in ["", "???$", "abc", "🙂", "____"] {
            assert!(QoderCliParser::decode_body_bytes(encoded.as_bytes()).is_none());
        }
    }

    #[test]
    fn translation_table_chains_the_swap_and_the_alphabet() {
        // Encode the same way the wire format does, then decode through the
        // table: swapping the thirds and substituting the alphabet must be
        // exact inverses for every standard Base64 character.
        fn encode(payload: &[u8]) -> String {
            let standard = STANDARD.encode(payload);
            let third = standard.len() / 3;
            let (head, rest) = standard.split_at(third);
            let (middle, tail) = rest.split_at(rest.len() - third);
            let mut wire = String::with_capacity(standard.len());
            for byte in tail.bytes().chain(middle.bytes()).chain(head.bytes()) {
                wire.push(if byte == b'=' {
                    '$'
                } else {
                    let index = STANDARD_ALPHABET
                        .iter()
                        .position(|c| *c == byte)
                        .expect("standard alphabet");
                    ALPHABET[index] as char
                });
            }
            wire
        }
        for payload in [
            b"f".as_slice(),
            b"fo",
            b"foo",
            b"{\"messages\":[{\"role\":\"user\",\"content\":\"hi\"}]}",
        ] {
            assert_eq!(
                qoder_decode(&encode(payload)),
                Some(payload.to_vec()),
                "{payload:?}"
            );
        }
    }

    #[test]
    fn preserve_system_model_tools_and_parameters() {
        let body = json!({
            "system": "Be concise", "messages": [{"role":"user","content":"hello"}],
            "model_config":{"model":"","key":"fixture-model"},
            "parameters":{"max_tokens":32,"temperature":0.5,"model":"wrong","messages":[]},
            "tools":[{"type":"function","function":{"name":"lookup","parameters":{}}}]
        });
        let parsed = QoderCliParser::parse_request(&body).unwrap();
        assert_eq!(parsed.model, "fixture-model");
        assert_eq!(parsed.max_tokens, Some(32));
        assert_eq!(
            parsed.messages[0].content.as_ref().unwrap().as_text(),
            "Be concise"
        );
        assert_eq!(parsed.tools.unwrap().len(), 1);
        assert!(QoderCliParser::parse_request(&json!({})).is_none());
    }

    #[test]
    fn reject_query_lookalikes_and_unmarked_bodies() {
        assert!(!QoderCliParser::matches_path(
            "/health?next=/algo/api/v2/service/pro/sse/agent_chat_generation"
        ));
        for value in [
            json!({"body": "{\"usage\":{\"prompt_tokens\":99}}"}),
            json!({"statusCode": "OK", "body": "invalid JSON"}),
        ] {
            assert!(crate::parser::llm::unwrap_qoder_envelope(&value).is_none());
        }
    }
}
