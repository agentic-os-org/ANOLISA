//! Identity-scoped replay suppression for OpenAI Responses reasoning items.

use serde_json::Value;
use std::borrow::Borrow;
use std::collections::HashSet;

#[derive(Clone, Copy, Eq, Hash, PartialEq)]
enum ReasoningPart {
    Content(u64),
    Summary(u64),
}

/// Nonempty reasoning deltas indexed without sizing storage from wire indexes.
#[derive(Default)]
pub(crate) struct ResponsesReasoningReplay {
    by_item_id: HashSet<(String, ReasoningPart)>,
    by_output_index: HashSet<(u64, ReasoningPart)>,
    saw_delta: bool,
    saw_identity_less_delta: bool,
}

impl ResponsesReasoningReplay {
    pub(crate) fn new<T>(chunks: impl IntoIterator<Item = T>) -> Self
    where
        T: Borrow<Value>,
    {
        let mut replay = Self::default();
        for chunk in chunks {
            replay.index_delta(chunk.borrow());
        }
        replay
    }

    fn index_delta(&mut self, chunk: &Value) {
        let part = match chunk.get("type").and_then(Value::as_str) {
            Some("response.reasoning_text.delta") => ReasoningPart::Content(
                chunk
                    .get("content_index")
                    .and_then(Value::as_u64)
                    .unwrap_or(0),
            ),
            Some("response.reasoning_summary_text.delta") => ReasoningPart::Summary(
                chunk
                    .get("summary_index")
                    .and_then(Value::as_u64)
                    .unwrap_or(0),
            ),
            _ => return,
        };
        if chunk
            .get("delta")
            .and_then(Value::as_str)
            .is_none_or(|delta| delta.is_empty())
        {
            return;
        }

        self.saw_delta = true;
        let item_id = nonempty_str(chunk.get("item_id"));
        let output_index = chunk.get("output_index").and_then(Value::as_u64);
        let mut identified = false;
        if let Some(id) = item_id {
            self.by_item_id.insert((id.to_owned(), part));
            identified = true;
        }
        if let Some(index) = output_index {
            self.by_output_index.insert((index, part));
            identified = true;
        }
        if !identified {
            self.saw_identity_less_delta = true;
        }
    }

    /// Return only the closing text that was not already delivered as a delta.
    ///
    /// Identity-less events retain the historical response-wide suppression,
    /// since there is no safe way to associate their deltas with one item.
    pub(crate) fn closing_item_text(&self, chunk: &Value) -> Option<String> {
        let item = chunk.get("item")?;
        if item.get("type").and_then(Value::as_str) != Some("reasoning") {
            return None;
        }
        let item_id = nonempty_str(item.get("id"));
        let output_index = chunk.get("output_index").and_then(Value::as_u64);
        let mut text = String::new();
        for (field, summary) in [("content", false), ("summary", true)] {
            if let Some(parts) = item.get(field).and_then(Value::as_array) {
                for (position, part) in parts.iter().enumerate() {
                    let position = position as u64;
                    let key = if summary {
                        ReasoningPart::Summary(position)
                    } else {
                        ReasoningPart::Content(position)
                    };
                    if !self.replays(item_id, output_index, key) {
                        if let Some(part_text) = part.get("text").and_then(Value::as_str) {
                            text.push_str(part_text);
                        }
                    }
                }
            }
        }
        (!text.is_empty()).then_some(text)
    }

    fn replays(
        &self,
        item_id: Option<&str>,
        output_index: Option<u64>,
        part: ReasoningPart,
    ) -> bool {
        if self.saw_identity_less_delta {
            return true;
        }
        if item_id.is_none() && output_index.is_none() {
            return self.saw_delta;
        }
        item_id.is_some_and(|id| self.by_item_id.contains(&(id.to_owned(), part)))
            || output_index.is_some_and(|index| self.by_output_index.contains(&(index, part)))
    }
}

fn nonempty_str(value: Option<&Value>) -> Option<&str> {
    value
        .and_then(Value::as_str)
        .filter(|text| !text.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn closing_item_keeps_only_the_part_without_deltas() {
        let chunks = vec![
            json!({"type":"response.reasoning_summary_text.delta","item_id":"rs_0","output_index":0,"summary_index":0,"delta":"first"}),
            json!({"type":"response.output_item.done","output_index":0,"item":{"type":"reasoning","id":"rs_0","summary":[
                {"type":"summary_text","text":"first"},
                {"type":"summary_text","text":" second"}
            ]}}),
        ];
        let replay = ResponsesReasoningReplay::new(&chunks);
        assert_eq!(
            replay.closing_item_text(&chunks[1]).as_deref(),
            Some(" second")
        );
    }

    #[test]
    fn content_delta_does_not_suppress_summary_part() {
        let chunks = vec![
            json!({"type":"response.reasoning_text.delta","item_id":"rs_0","output_index":0,"content_index":0,"delta":"content"}),
            json!({"type":"response.output_item.done","output_index":0,"item":{"type":"reasoning","id":"rs_0","content":[{"type":"reasoning_text","text":"content"}],"summary":[{"type":"summary_text","text":"summary"}]}}),
        ];
        let replay = ResponsesReasoningReplay::new(&chunks);
        assert_eq!(
            replay.closing_item_text(&chunks[1]).as_deref(),
            Some("summary")
        );
    }

    #[test]
    fn empty_deltas_do_not_suppress_closing_text() {
        let chunks = vec![
            json!({"type":"response.reasoning_text.delta","item_id":"rs_0","output_index":0,"content_index":0,"delta":""}),
            json!({"type":"response.output_item.done","output_index":0,"item":{"type":"reasoning","id":"rs_0","content":[{"type":"reasoning_text","text":"kept"}]}}),
        ];
        let replay = ResponsesReasoningReplay::new(&chunks);
        assert_eq!(
            replay.closing_item_text(&chunks[1]).as_deref(),
            Some("kept")
        );
    }

    #[test]
    fn identity_less_events_keep_response_wide_suppression() {
        let chunks = vec![
            json!({"type":"response.reasoning_text.delta","delta":"delta"}),
            json!({"type":"response.output_item.done","output_index":0,"item":{"type":"reasoning","id":"rs_0","content":[{"type":"reasoning_text","text":"delta"}]}}),
        ];
        let replay = ResponsesReasoningReplay::new(&chunks);
        assert_eq!(replay.closing_item_text(&chunks[1]), None);

        let chunks = vec![
            json!({"type":"response.reasoning_text.delta","item_id":"rs_0","output_index":0,"content_index":0,"delta":"delta"}),
            json!({"type":"response.output_item.done","item":{"type":"reasoning","content":[{"type":"reasoning_text","text":"delta"}]}}),
        ];
        let replay = ResponsesReasoningReplay::new(&chunks);
        assert_eq!(replay.closing_item_text(&chunks[1]), None);
    }

    #[test]
    fn sparse_output_indexes_are_set_keys_not_allocation_sizes() {
        let chunks = vec![
            json!({"type":"response.output_item.done","output_index":0,"item":{"type":"reasoning","id":"rs_0","content":[{"type":"reasoning_text","text":"first"}]}}),
            json!({"type":"response.reasoning_text.delta","output_index":u64::MAX,"content_index":0,"delta":"sparse"}),
            json!({"type":"response.output_item.done","output_index":u64::MAX,"item":{"type":"reasoning","content":[{"type":"reasoning_text","text":"sparse"}]}}),
        ];
        let replay = ResponsesReasoningReplay::new(&chunks);
        assert_eq!(
            replay.closing_item_text(&chunks[0]).as_deref(),
            Some("first")
        );
        assert_eq!(replay.closing_item_text(&chunks[2]), None);
    }
}
