//! Heap payload accounting shared by deferred delivery and the FFI queue.

use super::semantic::{GenAISemanticEvent, LLMCall, MessagePart};
use serde_json::Value;
use std::mem::size_of;

use crate::utils::retained::json_bytes;

enum RequestField {
    Part { message: usize, part: usize },
    UserQuery,
}

struct BodyReference {
    event: usize,
    field: RequestField,
    source_part: Option<(usize, usize)>,
    range: std::ops::Range<usize>,
}

/// Deferred fields borrow exact byte ranges from their owned request payload.
/// Restore only the batch being delivered, so draining a queue does not inflate
/// every waiting call at once. Escaped or unmatched text stays owned unchanged.
pub(crate) struct DeferredPayload {
    events: Vec<GenAISemanticEvent>,
    references: Vec<BodyReference>,
}

impl From<Vec<GenAISemanticEvent>> for DeferredPayload {
    fn from(mut events: Vec<GenAISemanticEvent>) -> Self {
        let mut references = Vec::new();
        for (event, value) in events.iter_mut().enumerate() {
            let GenAISemanticEvent::LLMCall(call) = value else {
                continue;
            };
            let body = call.request.raw_body.as_deref();
            for (message, input) in call.request.messages.iter_mut().enumerate() {
                for (part, value) in input.parts.iter_mut().enumerate() {
                    let (MessagePart::Text { content } | MessagePart::Reasoning { content }) =
                        value
                    else {
                        continue;
                    };
                    if let Some(range) = body.and_then(|body| body_range(body, content)) {
                        references.push(BodyReference {
                            event,
                            field: RequestField::Part { message, part },
                            source_part: None,
                            range,
                        });
                        drop(std::mem::take(content));
                    }
                }
            }
            if let Some(query) = call.metadata.get_mut("user_query") {
                let location = body
                    .and_then(|body| body_range(body, query))
                    .map(|range| (None, range))
                    .or_else(|| {
                        call.request
                            .messages
                            .iter()
                            .enumerate()
                            .find_map(|(message, input)| {
                                input
                                    .parts
                                    .iter()
                                    .enumerate()
                                    .find_map(|(part, value)| match value {
                                        MessagePart::Text { content } => body_range(content, query)
                                            .map(|range| (Some((message, part)), range)),
                                        _ => None,
                                    })
                            })
                    });
                if let Some((source_part, range)) = location {
                    references.push(BodyReference {
                        event,
                        field: RequestField::UserQuery,
                        source_part,
                        range,
                    });
                    drop(std::mem::take(query));
                }
            }
        }
        Self { events, references }
    }
}

fn body_range(body: &str, text: &str) -> Option<std::ops::Range<usize>> {
    // Tiny fields are cheaper to keep than to track; exact matching preserves
    // nonstandard bodies and JSON escape sequences without reparsing them.
    if text.len() < 1024 {
        return None;
    }
    body.find(text).map(|start| start..start + text.len())
}

impl std::ops::Deref for DeferredPayload {
    type Target = [GenAISemanticEvent];

    fn deref(&self) -> &Self::Target {
        &self.events
    }
}

impl DeferredPayload {
    /// Estimate compact payload and reference allocations for queue admission.
    pub(crate) fn retained_bytes(&self) -> usize {
        self.events.capacity() * size_of::<GenAISemanticEvent>()
            + self
                .events
                .iter()
                .map(GenAISemanticEvent::retained_bytes)
                .sum::<usize>()
            + self.references.capacity() * size_of::<BodyReference>()
    }

    /// Resolve session metadata without changing the bodies referenced by fields.
    pub(crate) fn set_session_id(&mut self, session: &str) {
        for event in &mut self.events {
            if let GenAISemanticEvent::LLMCall(call) = event {
                call.metadata.insert("session_id".into(), session.into());
            }
        }
    }

    /// Restore every field before exposing the events to storage or callbacks.
    pub(crate) fn into_events(mut self) -> Vec<GenAISemanticEvent> {
        for reference in self.references {
            // Only this type mutates the compact payload, and its raw body,
            // message order, and part variants remain fixed until delivery.
            let GenAISemanticEvent::LLMCall(call) = &mut self.events[reference.event] else {
                unreachable!("body references belong to LLM calls");
            };
            let source = match reference.source_part {
                Some((message, part)) => match &call.request.messages[message].parts[part] {
                    MessagePart::Text { content } => content.as_str(),
                    _ => unreachable!("query references belong to text parts"),
                },
                None => match call.request.raw_body.as_deref() {
                    Some(body) => body,
                    None => unreachable!("raw-body references retain their source"),
                },
            };
            let text = source[reference.range].to_owned();
            match reference.field {
                RequestField::Part { message, part } => {
                    match &mut call.request.messages[message].parts[part] {
                        MessagePart::Text { content } | MessagePart::Reasoning { content } => {
                            *content = text;
                        }
                        _ => unreachable!("body references belong to text parts"),
                    }
                }
                RequestField::UserQuery => {
                    call.metadata.insert("user_query".into(), text);
                }
            }
        }
        self.events
    }
}

fn optional_string(value: &Option<String>) -> usize {
    value.as_ref().map_or(0, String::capacity)
}

fn parts_bytes(parts: &Vec<MessagePart>) -> usize {
    parts.capacity() * size_of::<MessagePart>()
        + parts
            .iter()
            .map(|part| match part {
                MessagePart::Text { content } | MessagePart::Reasoning { content } => {
                    content.capacity()
                }
                MessagePart::ToolCall {
                    id,
                    name,
                    arguments,
                } => {
                    optional_string(id) + name.capacity() + arguments.as_ref().map_or(0, json_bytes)
                }
                MessagePart::ToolCallResponse { id, response } => {
                    optional_string(id) + json_bytes(response)
                }
            })
            .sum::<usize>()
}

impl LLMCall {
    /// Estimate retained heap allocations, including raw bodies and spare capacity.
    pub(crate) fn retained_bytes(&self) -> usize {
        size_of::<Self>()
            + self.call_id.capacity()
            + self.provider.capacity()
            + self.model.capacity()
            + self.process_name.capacity()
            + optional_string(&self.agent_name)
            + optional_string(&self.error)
            + self.metadata.capacity() * size_of::<(String, String)>()
            + self
                .metadata
                .iter()
                .map(|(k, v)| k.capacity() + v.capacity())
                .sum::<usize>()
            + self.request.messages.capacity() * size_of::<super::semantic::InputMessage>()
            + self
                .request
                .messages
                .iter()
                .map(|m| m.role.capacity() + optional_string(&m.name) + parts_bytes(&m.parts))
                .sum::<usize>()
            + self.response.messages.capacity() * size_of::<super::semantic::OutputMessage>()
            + self
                .response
                .messages
                .iter()
                .map(|m| {
                    m.role.capacity()
                        + optional_string(&m.name)
                        + optional_string(&m.finish_reason)
                        + parts_bytes(&m.parts)
                })
                .sum::<usize>()
            + optional_string(&self.request.raw_body)
            + optional_string(&self.response.raw_body)
            + self.request.tools.as_ref().map_or(0, |tools| {
                tools.capacity() * size_of::<Value>() + tools.iter().map(json_bytes).sum::<usize>()
            })
            + self.request.stop_sequences.as_ref().map_or(0, |values| {
                values.capacity() * size_of::<String>()
                    + values.iter().map(String::capacity).sum::<usize>()
            })
    }
}

impl GenAISemanticEvent {
    /// Estimate owned payloads without serializing or copying the event.
    pub(crate) fn retained_bytes(&self) -> usize {
        match self {
            Self::LLMCall(call) => call.retained_bytes(),
            Self::ToolUse(tool) => {
                size_of::<Self>()
                    + tool.tool_use_id.capacity()
                    + tool.tool_name.capacity()
                    + json_bytes(&tool.arguments)
                    + optional_string(&tool.result)
                    + optional_string(&tool.error)
                    + optional_string(&tool.parent_llm_call_id)
            }
            Self::AgentInteraction(event) => {
                size_of::<Self>()
                    + event.interaction_id.capacity()
                    + event.agent_name.capacity()
                    + event.interaction_type.capacity()
                    + event.content.capacity()
                    + optional_string(&event.parent_llm_call_id)
            }
            Self::StreamChunk(event) => {
                size_of::<Self>()
                    + event.stream_id.capacity()
                    + event.content.capacity()
                    + event.parent_llm_call_id.capacity()
            }
        }
    }
}
