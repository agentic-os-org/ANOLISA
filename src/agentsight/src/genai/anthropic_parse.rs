//! GenAI Anthropic Messages SSE merging for the drain path.
//!
//! Counterpart of [`openai_parse`]'s `merge_sse_chunks` (OpenAI wire shape):
//! when a traced agent dies mid-call, [`super::GenAIBuilder::extract_sse_enrichment`]
//! must fold the captured SSE events into the same OTel parts shape the live
//! path persists. Anthropic streams carry no `choices[]` — text, thinking and
//! tool arguments ride in block-indexed `content_block_delta` events — so the
//! OpenAI merger yields nothing and this module rebuilds the blocks instead.
//!
//! The live path's `AnthropicParser::aggregate_sse_events` is not reused here
//! because it returns content blocks (`AnthropicResponse`), while the drain
//! path must persist `MessagePart`s exactly like the OpenAI drain merger.

use super::semantic::MessagePart;
use std::collections::HashMap;

/// Block state while streaming; keyed by the wire `index` so a skipped or
/// reordered block cannot cross-contaminate a neighbour.
enum StreamingBlock {
    Text(String),
    Thinking(String),
    ToolUse {
        id: Option<String>,
        name: String,
        json: String,
    },
}

/// Merge Anthropic Messages SSE events into `MessagePart`s + finish_reason.
///
/// Returns empty parts when no Anthropic event tags are present, so callers
/// can use "OpenAI merger found nothing" → "try this one" without a separate
/// provider sniff.
pub(super) fn merge_anthropic_sse_chunks(
    chunks: &[serde_json::Value],
) -> (Vec<MessagePart>, Option<String>) {
    let mut blocks: HashMap<u64, StreamingBlock> = HashMap::new();
    // First-seen order of indexes: Anthropic streams blocks in index order,
    // but the map alone would lose it.
    let mut order: Vec<u64> = Vec::new();
    let mut finish_reason: Option<String> = None;
    let mut saw_anthropic_event = false;

    for chunk in chunks {
        let ty = chunk.get("type").and_then(|v| v.as_str()).unwrap_or("");
        match ty {
            "content_block_start" => {
                saw_anthropic_event = true;
                let Some(idx) = chunk.get("index").and_then(|v| v.as_u64()) else {
                    continue;
                };
                let cb = chunk
                    .get("content_block")
                    .cloned()
                    .unwrap_or(serde_json::Value::Null);
                let block = match cb.get("type").and_then(|v| v.as_str()) {
                    Some("tool_use") => StreamingBlock::ToolUse {
                        id: cb.get("id").and_then(|v| v.as_str()).map(str::to_string),
                        name: cb
                            .get("name")
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_string(),
                        json: String::new(),
                    },
                    Some("thinking") => StreamingBlock::Thinking(String::new()),
                    // text blocks and future types: a text-bearing delta
                    // still lands in Text, anything else stays empty and is
                    // dropped at finalize time.
                    _ => StreamingBlock::Text(String::new()),
                };
                if !blocks.contains_key(&idx) {
                    order.push(idx);
                }
                blocks.insert(idx, block);
            }
            "content_block_delta" => {
                saw_anthropic_event = true;
                let Some(idx) = chunk.get("index").and_then(|v| v.as_u64()) else {
                    continue;
                };
                let Some(delta) = chunk.get("delta") else {
                    continue;
                };
                match delta.get("type").and_then(|v| v.as_str()) {
                    Some("text_delta") => {
                        if let Some(t) = delta.get("text").and_then(|v| v.as_str()) {
                            if let Some(StreamingBlock::Text(buf)) = blocks.get_mut(&idx) {
                                buf.push_str(t);
                            }
                        }
                    }
                    Some("thinking_delta") => {
                        if let Some(t) = delta.get("thinking").and_then(|v| v.as_str()) {
                            if let Some(StreamingBlock::Thinking(buf)) = blocks.get_mut(&idx) {
                                buf.push_str(t);
                            }
                        }
                    }
                    Some("input_json_delta") => {
                        if let Some(j) = delta.get("partial_json").and_then(|v| v.as_str()) {
                            if let Some(StreamingBlock::ToolUse { json, .. }) = blocks.get_mut(&idx)
                            {
                                json.push_str(j);
                            }
                        }
                    }
                    _ => {}
                }
            }
            "message_delta" => {
                saw_anthropic_event = true;
                if let Some(fr) = chunk.pointer("/delta/stop_reason").and_then(|v| v.as_str()) {
                    finish_reason = Some(fr.to_string());
                }
            }
            _ => {}
        }
    }
    if !saw_anthropic_event {
        return (Vec::new(), None);
    }

    // Finalize in first-seen order — matches the live path's block order.
    let mut parts = Vec::new();
    for idx in order {
        match blocks.remove(&idx) {
            Some(StreamingBlock::Text(t)) if !t.is_empty() => {
                parts.push(MessagePart::Text { content: t });
            }
            Some(StreamingBlock::Thinking(t)) if !t.is_empty() => {
                parts.push(MessagePart::Reasoning { content: t });
            }
            Some(StreamingBlock::ToolUse { id, name, json }) if !name.is_empty() => {
                parts.push(MessagePart::ToolCall {
                    id,
                    name,
                    // An interrupted stream can leave a truncated JSON
                    // fragment; None keeps the part while honestly marking
                    // the arguments unparsable.
                    arguments: serde_json::from_str(&json).ok(),
                });
            }
            _ => {}
        }
    }
    (parts, finish_reason)
}
