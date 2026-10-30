//! Coalesce provider deltas in place while preserving the existing parser shapes.

use crate::parser::sse::ParsedSseEvent;
use crate::parser::sse::parser::SseFrame;
use crate::probes::sslsniff::SslEvent;
use crate::utils::retained::json_bytes;
use serde_json::Value;
use std::collections::HashMap;

#[derive(Debug)]
enum Entry {
    Delta {
        value: Value,
        source: SslEvent,
        event: Option<String>,
    },
    Event(ParsedSseEvent),
}

#[derive(Debug, Hash, PartialEq, Eq)]
enum DeltaKey {
    Chat(u64),
    Native,
    Anthropic(u64, std::borrow::Cow<'static, str>),
    Responses(&'static str, Box<str>, [u64; 3]),
}

impl DeltaKey {
    fn allocated_bytes(&self) -> usize {
        match self {
            Self::Anthropic(_, std::borrow::Cow::Owned(kind)) => kind.capacity(),
            Self::Responses(_, item, _) => item.len(),
            _ => 0,
        }
    }
}

#[derive(Debug, Default)]
pub(super) struct SseReducer {
    entries: Vec<Entry>,
    slots: HashMap<DeltaKey, usize>,
    slot_bytes: usize,
    /// Missing request metadata keeps the legacy prefix-detection behavior.
    pub(super) native_incremental: Option<bool>,
    payload_bytes: usize,
    pub(super) count: usize,
}

impl SseReducer {
    pub(super) fn push(&mut self, frame: SseFrame<'_>, capture: &SslEvent) -> bool {
        self.count += 1;
        let mut value = needs_json(&frame.data)
            .then(|| serde_json::from_slice::<Value>(&frame.data).ok())
            .flatten();
        let done = frame.event.as_deref() == Some("message_stop")
            || (value.is_none() && text_is_done(&frame.data))
            || ParsedSseEvent::json_is_done(value.as_ref());
        let pruned = value.as_mut().is_some_and(prune_response_echo);
        if pruned {
            self.slots.clear();
            self.slot_bytes = 0;
            if let Some(value) = value {
                let source = capture.metadata_only();
                self.payload_bytes += json_bytes(&value)
                    + source.comm.capacity()
                    + frame.event.as_ref().map_or(0, |event| match event {
                        std::borrow::Cow::Borrowed(value) => value.len(),
                        std::borrow::Cow::Owned(value) => value.capacity(),
                    });
                self.entries.push(Entry::Delta {
                    value,
                    source,
                    event: frame.event.map(std::borrow::Cow::into_owned),
                });
            }
            return done;
        }
        if let Some((value, key)) = value
            .as_mut()
            .and_then(|value| key(value).map(|key| (value.take(), key)))
        {
            if let Some(index) = self.slots.get(&key) {
                if let Entry::Delta {
                    value: previous,
                    source,
                    ..
                } = &mut self.entries[*index]
                {
                    if !super::response::event_has_meaningful_output(Some(previous))
                        && super::response::event_has_meaningful_output(Some(&value))
                    {
                        let updated = capture.metadata_only();
                        self.payload_bytes =
                            self.payload_bytes.saturating_sub(source.comm.capacity())
                                + updated.comm.capacity();
                        *source = updated;
                    }
                    let delta = merge(
                        previous,
                        value,
                        "",
                        key == DeltaKey::Native,
                        self.native_incremental,
                    );
                    adjust_bytes(&mut self.payload_bytes, delta);
                    return done;
                }
            }
            let source = capture.metadata_only();
            self.payload_bytes += json_bytes(&value)
                + source.comm.capacity()
                + frame.event.as_ref().map_or(0, |event| match event {
                    std::borrow::Cow::Borrowed(value) => value.len(),
                    std::borrow::Cow::Owned(value) => value.capacity(),
                });
            self.slot_bytes += key.allocated_bytes();
            self.slots.insert(key, self.entries.len());
            self.entries.push(Entry::Delta {
                value,
                source,
                event: frame.event.map(std::borrow::Cow::into_owned),
            });
        } else {
            // Lifecycle events delimit delta groups: an unkeyed Responses tool
            // delta must never move across the next output_item.added event.
            self.slots.clear();
            self.slot_bytes = 0;
            let event = frame.into_event(capture, value);
            self.payload_bytes += event.source_event().buf.capacity()
                + event.parsed_retained_bytes()
                + std::mem::size_of::<SslEvent>()
                + event.source_event().comm.capacity()
                + event.event.as_ref().map_or(0, String::capacity)
                + event.id.as_ref().map_or(0, String::capacity);
            self.entries.push(Entry::Event(event));
        }
        done
    }

    pub(super) fn retained_bytes(&self) -> usize {
        self.payload_bytes
            + self.entries.capacity() * std::mem::size_of::<Entry>()
            + self.slots.capacity() * std::mem::size_of::<(DeltaKey, usize)>()
            + self.slot_bytes
    }

    pub(super) fn into_events(self) -> Vec<ParsedSseEvent> {
        self.entries
            .into_iter()
            .map(|entry| match entry {
                Entry::Event(event) => event,
                Entry::Delta {
                    value,
                    source,
                    event,
                } => ParsedSseEvent::from_json(None, event, None, value, source),
            })
            .collect()
    }
}

fn text_is_done(data: &[u8]) -> bool {
    let data = data.trim_ascii();
    matches!(data, b"[DONE]" | b"[END]")
        || ((data.first().is_some_and(|byte| !byte.is_ascii())
            || data.last().is_some_and(|byte| !byte.is_ascii()))
            && std::str::from_utf8(data)
                .is_ok_and(|data| matches!(data.trim(), "[DONE]" | "[END]")))
}

// Escaped field names can encode every interesting key; conservatively parse
// those frames rather than treating a literal-key miss as evidence of absence.
fn needs_json(mut data: &[u8]) -> bool {
    while let Some(offset) = memchr::memchr2(b'"', b'\\', data) {
        data = &data[offset..];
        if data[0] == b'\\' {
            return true;
        }
        data = &data[1..];
        if [
            b"delta\"".as_slice(),
            b"type\"",
            b"output\"",
            b"finishReason\"",
        ]
        .iter()
        .any(|key| data.starts_with(key))
        {
            return true;
        }
    }
    false
}

fn prune_response_echo(value: &mut Value) -> bool {
    if !matches!(
        value.get("type").and_then(Value::as_str),
        Some(
            "response.created"
                | "response.in_progress"
                | "response.completed"
                | "response.failed"
                | "response.incomplete"
        )
    ) {
        return false;
    }
    let Some(response) = value.get_mut("response").and_then(Value::as_object_mut) else {
        return false;
    };
    let mut removed = false;
    for field in ["instructions", "input", "tools", "prompt"] {
        removed |= response.remove(field).is_some();
    }
    removed
}

fn key(value: &Value) -> Option<DeltaKey> {
    if let Some(choices) = value.get("choices").and_then(Value::as_array) {
        if choices.len() == 1 && choices[0].get("delta").is_some_and(Value::is_object) {
            // Missing indexes cannot identify sequential or parallel tool calls.
            // Keep their event boundaries for the existing ID-aware parsers.
            if choices[0]["delta"]
                .get("tool_calls")
                .and_then(Value::as_array)
                .is_some_and(|calls| {
                    calls
                        .iter()
                        .any(|call| call.get("index").and_then(Value::as_u64).is_none())
                })
            {
                return None;
            }
            return Some(DeltaKey::Chat(
                choices[0].get("index").and_then(Value::as_u64).unwrap_or(0),
            ));
        }
    }
    if let Some(output) = value.get("output") {
        let rich_content = output
            .get("choices")
            .and_then(Value::as_array)
            .is_some_and(|choices| {
                choices.iter().any(|choice| {
                    choice
                        .get("message")
                        .or_else(|| choice.get("delta"))
                        .and_then(|message| message.get("content"))
                        .is_some_and(Value::is_array)
                })
            });
        if !rich_content && (output.get("text").is_some() || output.get("choices").is_some()) {
            return Some(DeltaKey::Native);
        }
    }
    let kind = value.get("type")?.as_str()?;
    if kind == "content_block_delta" {
        let kind = value.get("delta")?.get("type")?.as_str()?;
        let kind = match kind {
            "text_delta" => std::borrow::Cow::Borrowed("text_delta"),
            "thinking_delta" => std::borrow::Cow::Borrowed("thinking_delta"),
            "input_json_delta" => std::borrow::Cow::Borrowed("input_json_delta"),
            "signature_delta" => std::borrow::Cow::Borrowed("signature_delta"),
            other => std::borrow::Cow::Owned(other.to_owned()),
        };
        return Some(DeltaKey::Anthropic(value.get("index")?.as_u64()?, kind));
    }
    let kind = match kind {
        "response.output_text.delta" => "response.output_text.delta",
        "response.reasoning_text.delta" => "response.reasoning_text.delta",
        "response.reasoning_summary_text.delta" => "response.reasoning_summary_text.delta",
        "response.refusal.delta" => "response.refusal.delta",
        "response.function_call_arguments.delta" => "response.function_call_arguments.delta",
        "response.custom_tool_call_input.delta" => "response.custom_tool_call_input.delta",
        _ => return None,
    };
    Some(DeltaKey::Responses(
        kind,
        value
            .get("item_id")
            .and_then(Value::as_str)
            .unwrap_or("")
            .into(),
        [
            value
                .get("output_index")
                .and_then(Value::as_u64)
                .unwrap_or(0),
            value
                .get("content_index")
                .and_then(Value::as_u64)
                .unwrap_or(0),
            value
                .get("summary_index")
                .and_then(Value::as_u64)
                .unwrap_or(0),
        ],
    ))
}

fn adjust_bytes(bytes: &mut usize, delta: isize) {
    if delta < 0 {
        *bytes = bytes.saturating_sub(delta.unsigned_abs());
    } else {
        *bytes += delta as usize;
    }
}

fn merge(
    previous: &mut Value,
    incoming: Value,
    field: &str,
    native: bool,
    native_incremental: Option<bool>,
) -> isize {
    match (previous, incoming) {
        (Value::Object(previous), Value::Object(incoming)) => {
            let mut delta = 0;
            for (key, value) in incoming {
                if let Some(old) = previous.get_mut(&key) {
                    delta += merge(old, value, &key, native, native_incremental);
                } else {
                    delta += (std::mem::size_of::<(String, Value)>()
                        + key.capacity()
                        + json_bytes(&value)) as isize;
                    previous.insert(key, value);
                }
            }
            delta
        }
        (previous, incoming @ Value::Array(_)) if native && field == "tool_calls" => {
            let delta = json_bytes(&incoming) as isize - json_bytes(previous) as isize;
            *previous = incoming;
            delta
        }
        (Value::Array(previous), Value::Array(incoming))
            if matches!(field, "choices" | "tool_calls") =>
        {
            let capacity = previous.capacity();
            let mut delta = 0;
            for value in incoming {
                let index = value.get("index").and_then(Value::as_u64).unwrap_or(0);
                if let Some(old) = previous
                    .iter_mut()
                    .find(|old| old.get("index").and_then(Value::as_u64).unwrap_or(0) == index)
                {
                    delta += merge(old, value, "", native, native_incremental);
                } else {
                    delta += json_bytes(&value) as isize;
                    previous.push(value);
                }
            }
            delta + ((previous.capacity() - capacity) * std::mem::size_of::<Value>()) as isize
        }
        (Value::String(previous), Value::String(incoming))
            if matches!(
                field,
                "content"
                    | "reasoning_content"
                    | "refusal"
                    | "arguments"
                    | "text"
                    | "thinking"
                    | "partial_json"
                    | "delta"
            ) =>
        {
            let capacity = previous.capacity();
            if native
                && matches!(field, "content" | "text" | "reasoning_content")
                && !incoming.is_empty()
                && match native_incremental {
                    Some(incremental) => !incremental,
                    None => incoming.starts_with(previous.as_str()),
                }
            {
                *previous = incoming;
            } else {
                previous.push_str(&incoming);
            }
            previous.capacity() as isize - capacity as isize
        }
        (Value::Number(previous), Value::Number(incoming))
            if field.ends_with("tokens") || field.ends_with("token_count") =>
        {
            if incoming.as_u64().unwrap_or(0) > previous.as_u64().unwrap_or(0) {
                *previous = incoming;
            }
            0
        }
        (_, Value::Null) => 0,
        (_, Value::String(value)) if value.is_empty() => 0,
        (previous, incoming) => {
            let delta = json_bytes(&incoming) as isize - json_bytes(previous) as isize;
            *previous = incoming;
            delta
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn push_bytes(reducer: &mut SseReducer, data: &[u8]) -> bool {
        let bytes = [b"data: ".as_slice(), data, b"\n\n"].concat();
        let len = bytes.len();
        let source = SslEvent {
            source: 0,
            timestamp_ns: 1,
            delta_ns: 0,
            pid: 1,
            tid: 1,
            uid: 0,
            len: len as u32,
            rw: 0,
            comm: "fixture".into(),
            buf: bytes,
            is_handshake: false,
            ssl_ptr: 1,
        };
        reducer.push(SseFrame::parse(&source.buf, false).unwrap(), &source)
    }

    fn push(reducer: &mut SseReducer, value: Value) -> bool {
        push_bytes(reducer, value.to_string().as_bytes())
    }

    #[test]
    fn merge_accounting_matches_full_json_estimates_after_every_delta() {
        let mut reducer = SseReducer::default();
        for n in 0..80 {
            push(
                &mut reducer,
                json!({"choices":[{"index":0,"delta":{"content":"汉字🙂", "tool_calls":[{"index":n % 3,"function":{"name":"tool","arguments":"fragment"}}]}}], "usage":{"completion_tokens":n}}),
            );
            let expected = reducer
                .entries
                .iter()
                .map(|entry| match entry {
                    Entry::Delta {
                        value,
                        source,
                        event,
                    } => {
                        json_bytes(value)
                            + source.comm.capacity()
                            + event.as_ref().map_or(0, String::capacity)
                    }
                    Entry::Event(event) => {
                        std::mem::size_of::<SslEvent>()
                            + event.source_event().buf.capacity()
                            + event.source_event().comm.capacity()
                            + event.parsed_retained_bytes()
                            + event_metadata(event)
                    }
                })
                .sum::<usize>();
            assert_eq!(reducer.payload_bytes, expected);
            assert_eq!(
                reducer.slot_bytes,
                reducer
                    .slots
                    .keys()
                    .map(DeltaKey::allocated_bytes)
                    .sum::<usize>()
            );
        }
    }

    fn event_metadata(event: &ParsedSseEvent) -> usize {
        event.id.as_ref().map_or(0, String::capacity)
            + event.event.as_ref().map_or(0, String::capacity)
    }

    #[test]
    fn native_explicit_modes_and_unknown_mode_keep_distinct_semantics() {
        for (mode, expected) in [(None, "aa"), (Some(false), "aa"), (Some(true), "aaa")] {
            let mut reducer = SseReducer {
                native_incremental: mode,
                ..Default::default()
            };
            push(&mut reducer, json!({"output":{"text":"a"}}));
            push(&mut reducer, json!({"output":{"text":"aa"}}));
            let events = reducer.into_events();
            assert_eq!(events[0].json_body().unwrap()["output"]["text"], expected);
            assert!(events[0].source_event().buf.is_empty());
        }
    }

    #[test]
    fn unkeyed_frames_skip_preparse_and_selected_frames_keep_the_parsed_value() {
        let mut reducer = SseReducer::default();
        let gemini = br#"{"candidates":[{"content":{"parts":[{"text":"hello"}]}}]}"#;
        assert!(!needs_json(gemini));
        assert!(!push_bytes(&mut reducer, gemini));
        let terminal = br#"{"candidates":[{"finishReason":"STOP"}]}"#;
        assert!(push_bytes(&mut reducer, terminal));
        let events = reducer.into_events();
        assert_eq!(events[0].data(), gemini);
        assert_eq!(events[0].parsed_retained_bytes(), 0);
        assert!(events[1].source_event().buf.is_empty());
        assert!(events[1].parsed_retained_bytes() > 0);
        assert_eq!(
            events[1].json_body().unwrap()["candidates"][0]["finishReason"],
            "STOP"
        );
    }

    #[test]
    fn escaped_keys_and_terminators_remain_exact() {
        for bytes in [
            br#"{"\u0074ype":"message_stop"}"#.as_slice(),
            br#"{"\u006futput":{"finish_reason":"stop"}}"#,
            br#"{"candidates":[{"finish\u0052eason":"STOP"}]}"#,
            b" [DONE] ",
            "\u{2003}[END]\u{2003}".as_bytes(),
        ] {
            assert!(push_bytes(&mut SseReducer::default(), bytes), "{bytes:?}");
        }
        let mut reducer = SseReducer::default();
        assert!(!push_bytes(
            &mut reducer,
            br#"{"type":"response.completed""#
        ));
        assert!(!push_bytes(&mut reducer, b"[DONE]\xff"));
        assert_eq!(reducer.into_events().len(), 2);
    }

    #[test]
    fn unkeyed_passthrough_remains_a_delta_group_barrier() {
        let mut reducer = SseReducer::default();
        push(
            &mut reducer,
            json!({"choices":[{"delta":{"content":"first"}}]}),
        );
        push(&mut reducer, json!({"marker":"barrier"}));
        push(
            &mut reducer,
            json!({"choices":[{"delta":{"content":"second"}}]}),
        );
        let events = reducer.into_events();
        assert_eq!(events.len(), 3);
        assert_eq!(
            events[0].json_body().unwrap()["choices"][0]["delta"]["content"],
            "first"
        );
        assert_eq!(events[1].json_body().unwrap()["marker"], "barrier");
        assert_eq!(
            events[2].json_body().unwrap()["choices"][0]["delta"]["content"],
            "second"
        );
    }

    #[test]
    fn unknown_native_mode_keeps_per_event_compatibility_including_mode_switches() {
        let mut reducer = SseReducer::default();
        for (input, expected) in [("a", "a"), ("ab", "ab"), ("X", "abX"), ("abXz", "abXz")] {
            push(&mut reducer, json!({"output":{"text":input}}));
            let Entry::Delta { value, .. } = &reducer.entries[0] else {
                panic!("native text was not reduced")
            };
            assert_eq!(value["output"]["text"], expected);
        }
    }
}

#[cfg(test)]
mod provider_compatibility_tests {
    use super::*;
    use crate::parser::sse::SseParser;
    use std::borrow::Cow;
    use std::rc::Rc;

    fn parsed_events(body: &str, ssl_ptr: u64) -> Vec<ParsedSseEvent> {
        let source = Rc::new(SslEvent {
            source: 0,
            timestamp_ns: 1_000,
            delta_ns: 0,
            pid: 1,
            tid: 1,
            uid: 0,
            len: body.len() as u32,
            rw: 1,
            comm: "fixture".into(),
            buf: body.as_bytes().to_vec(),
            is_handshake: false,
            ssl_ptr,
        });
        SseParser::new().parse(source)
    }

    fn reduced(body: &str) -> Vec<Value> {
        let mut reducer = SseReducer::default();
        for event in parsed_events(body, 0x1) {
            let frame = SseFrame {
                id: event.id.as_deref().map(Cow::Borrowed),
                event: event.event.as_deref().map(Cow::Borrowed),
                retry: event.retry,
                data: Cow::Borrowed(event.data()),
            };
            reducer.push(frame, event.source_event());
        }
        reducer
            .into_events()
            .iter()
            .map(|event| event.json_body().expect("reduced events carry JSON"))
            .collect()
    }

    /// Two captures of the same custom-tool stream must reduce to the same
    /// shape a function-call stream does. The delta kinds that get a reduce
    /// key omitted `response.custom_tool_call_input.delta`, so consecutive
    /// input fragments stayed separate events instead of coalescing into one.
    #[test]
    fn custom_tool_input_deltas_reduce_like_function_arguments() {
        let custom = reduced(concat!(
            "data: {\"type\":\"response.output_item.added\",\"output_index\":0,",
            "\"item\":{\"type\":\"custom_tool_call\",\"id\":\"ctc_1\",",
            "\"call_id\":\"call_1\",\"name\":\"apply_patch\",\"input\":\"\"}}\n\n",
            "data: {\"type\":\"response.custom_tool_call_input.delta\",",
            "\"output_index\":0,\"item_id\":\"ctc_1\",\"delta\":\"*** Begin Patch\\n\"}\n\n",
            "data: {\"type\":\"response.custom_tool_call_input.delta\",",
            "\"output_index\":0,\"item_id\":\"ctc_1\",\"delta\":\"*** End Patch\"}\n\n",
        ));
        let function = reduced(concat!(
            "data: {\"type\":\"response.output_item.added\",\"output_index\":0,",
            "\"item\":{\"type\":\"function_call\",\"id\":\"fc_1\",",
            "\"call_id\":\"call_1\",\"name\":\"apply_patch\",\"arguments\":\"\"}}\n\n",
            "data: {\"type\":\"response.function_call_arguments.delta\",",
            "\"output_index\":0,\"item_id\":\"fc_1\",\"delta\":\"*** Begin Patch\\n\"}\n\n",
            "data: {\"type\":\"response.function_call_arguments.delta\",",
            "\"output_index\":0,\"item_id\":\"fc_1\",\"delta\":\"*** End Patch\"}\n\n",
        ));
        assert_eq!(
            custom.len(),
            function.len(),
            "a custom-tool stream must reduce to the same event count: {custom:?}"
        );
        assert_eq!(
            custom.last().and_then(|event| event.get("delta")),
            function.last().and_then(|event| event.get("delta")),
            "both streams must end with the same coalesced payload: {custom:?}"
        );
        assert_eq!(
            custom.last().and_then(|event| event.get("delta")),
            Some(&Value::String("*** Begin Patch\n*** End Patch".to_string()))
        );
    }
}
