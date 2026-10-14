//! Coalesce provider deltas in place while preserving the existing parser shapes.

use crate::parser::sse::ParsedSseEvent;
use crate::probes::sslsniff::SslEvent;
use crate::utils::retained::json_bytes;
use serde_json::Value;
use std::collections::HashMap;
use std::rc::Rc;

#[derive(Debug)]
enum Entry {
    Delta {
        value: Value,
        source: SslEvent,
        event: Option<String>,
    },
    Event(ParsedSseEvent),
}

#[derive(Debug, Default)]
pub(super) struct SseReducer {
    entries: Vec<Entry>,
    slots: HashMap<String, usize>,
    payload_bytes: usize,
    pub(super) count: usize,
}

impl SseReducer {
    pub(super) fn push(&mut self, event: ParsedSseEvent) -> bool {
        self.count += 1;
        let mut value = event.json_body();
        let done = event.is_done_with_json(value.as_ref());
        let pruned = value.as_mut().is_some_and(prune_response_echo);
        if pruned {
            self.slots.clear();
            if let Some(value) = value {
                self.payload_bytes += json_bytes(&value)
                    + event.source_event().comm.capacity()
                    + event.event.as_ref().map_or(0, String::capacity);
                self.entries.push(Entry::Delta {
                    value,
                    source: event.source_event().metadata_only(),
                    event: event.event,
                });
            }
            return done;
        }
        if let Some((value, key)) = value.and_then(|value| key(&value).map(|key| (value, key))) {
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
                        *source = event.source_event().metadata_only();
                    }
                    let before = json_bytes(previous);
                    merge(previous, value, "", key == "native");
                    self.payload_bytes =
                        self.payload_bytes.saturating_sub(before) + json_bytes(previous);
                    return done;
                }
            }
            self.payload_bytes += json_bytes(&value)
                + event.source_event().comm.capacity()
                + event.event.as_ref().map_or(0, String::capacity);
            self.slots.insert(key, self.entries.len());
            self.entries.push(Entry::Delta {
                value,
                source: event.source_event().metadata_only(),
                event: event.event,
            });
        } else {
            // Lifecycle events delimit delta groups: an unkeyed Responses tool
            // delta must never move across the next output_item.added event.
            self.slots.clear();
            self.payload_bytes += event.source_event().buf.capacity()
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
            + self.slots.capacity() * std::mem::size_of::<(String, usize)>()
            + self.slots.keys().map(String::capacity).sum::<usize>()
    }

    pub(super) fn into_events(self) -> Vec<ParsedSseEvent> {
        self.entries
            .into_iter()
            .filter_map(|entry| match entry {
                Entry::Event(event) => Some(event),
                Entry::Delta {
                    value,
                    mut source,
                    event,
                } => match serde_json::to_vec(&value) {
                    Ok(bytes) => {
                        let len = bytes.len();
                        source.buf = bytes;
                        source.len = len as u32;
                        Some(ParsedSseEvent::new(
                            None,
                            event,
                            None,
                            0,
                            len,
                            Rc::new(source),
                        ))
                    }
                    Err(error) => {
                        log::warn!("Cannot serialize reduced SSE event: {error}");
                        None
                    }
                },
            })
            .collect()
    }
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

fn key(value: &Value) -> Option<String> {
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
            return Some(format!(
                "chat:{}",
                choices[0].get("index").and_then(Value::as_u64).unwrap_or(0)
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
            return Some("native".into());
        }
    }
    let kind = value.get("type")?.as_str()?;
    if kind == "content_block_delta" {
        return Some(format!(
            "anthropic:{}:{}",
            value.get("index")?.as_u64()?,
            value.get("delta")?.get("type")?.as_str()?
        ));
    }
    if matches!(
        kind,
        "response.output_text.delta"
            | "response.reasoning_text.delta"
            | "response.reasoning_summary_text.delta"
            | "response.refusal.delta"
            | "response.function_call_arguments.delta"
    ) {
        return Some(format!(
            "{kind}:{}:{}:{}:{}",
            value.get("item_id").and_then(Value::as_str).unwrap_or(""),
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
                .unwrap_or(0)
        ));
    }
    None
}

fn merge(previous: &mut Value, incoming: Value, field: &str, native: bool) {
    match (previous, incoming) {
        (Value::Object(previous), Value::Object(incoming)) => {
            for (key, value) in incoming {
                if let Some(old) = previous.get_mut(&key) {
                    merge(old, value, &key, native);
                } else {
                    previous.insert(key, value);
                }
            }
        }
        (previous, incoming @ Value::Array(_)) if native && field == "tool_calls" => {
            *previous = incoming
        }
        (Value::Array(previous), Value::Array(incoming))
            if matches!(field, "choices" | "tool_calls") =>
        {
            for value in incoming {
                let index = value.get("index").and_then(Value::as_u64).unwrap_or(0);
                if let Some(old) = previous
                    .iter_mut()
                    .find(|old| old.get("index").and_then(Value::as_u64).unwrap_or(0) == index)
                {
                    merge(old, value, "", native);
                } else {
                    previous.push(value);
                }
            }
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
            if native
                && matches!(field, "content" | "text" | "reasoning_content")
                && incoming.starts_with(previous.as_str())
            {
                previous.clear();
            }
            previous.push_str(&incoming);
        }
        (Value::Number(previous), Value::Number(incoming))
            if field.ends_with("tokens") || field.ends_with("token_count") =>
        {
            if incoming.as_u64().unwrap_or(0) > previous.as_u64().unwrap_or(0) {
                *previous = incoming;
            }
        }
        (previous, Value::Null) => {
            let _ = previous;
        }
        (previous, Value::String(value)) if value.is_empty() => {
            let _ = previous;
        }
        (previous, incoming) => *previous = incoming,
    }
}
