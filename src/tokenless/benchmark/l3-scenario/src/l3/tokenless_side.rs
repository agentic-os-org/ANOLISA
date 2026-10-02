// Copyright 2026 Alibaba Cloud
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

//! The tokenless side of the comparison.
//!
//! Mirrors what tokenless actually ships, not a hypothetical
//! whole-conversation compressor. Two of its four compressors are reachable
//! from a committed conversation:
//!
//! - `SchemaCompressor` over the tools array, as its `BeforeModel` hook does.
//! - `ResponseCompressor` over each JSON tool response, as its `PostToolUse`
//!   hook does.
//!
//! The other two are out of scope for these assets by construction: TOON is a
//! pipeline step chained after `ResponseCompressor` (measured at L1, and it only
//! pays off on uniform tabular payloads), and rtk rewrites shell commands, which
//! these conversations do not carry.
//!
//! Both compressors run with default configuration — the same settings L1 and L2
//! measure — so the three layers stay on one basis.

use std::time::Instant;

use serde_json::Value;
use tokenless_schema::{ResponseCompressor, SchemaCompressor};

use super::asset::{Message, Scenario};
use super::tokenizer::{TokenCount, Tokenizers};

/// What tokenless did to one scenario.
#[derive(Debug, Clone)]
pub struct TokenlessResult {
    /// The conversation as tokenless would hand it to the model.
    pub messages: Vec<Message>,
    /// Tools array after schema compaction, when the scenario had one.
    pub tools: Option<Vec<Value>>,
    /// Tokens across the whole conversation before compression.
    pub before: TokenCount,
    /// Tokens across the whole conversation after compression.
    pub after: TokenCount,
    /// Wall time inside the compress calls, summed over the conversation.
    ///
    /// In-process: excludes tokenization and asset loading, so it measures the
    /// compressors rather than the harness.
    pub compress_ms: f64,
    /// How many messages were actually rewritten.
    pub messages_compressed: usize,
    /// How many tool schemas were compacted.
    pub tools_compressed: usize,
}

/// Run tokenless over a scenario.
///
/// Messages the compressors have no entry point for are passed through
/// byte-identical, so `before`/`after` stay comparable with the reference side,
/// which receives the same conversation.
pub fn run(scenario: &Scenario, tk: &Tokenizers) -> TokenlessResult {
    let response = ResponseCompressor::new();
    let schema = SchemaCompressor::new();

    let mut elapsed = 0.0_f64;
    let mut messages_compressed = 0usize;

    let mut out_messages = Vec::with_capacity(scenario.messages.len());
    for message in &scenario.messages {
        // Only a tool message whose content parses as JSON is a valid input;
        // anything else is what the production hook skips and the CLI rejects.
        let compressible = message.get("role").and_then(Value::as_str) == Some("tool");
        let parsed = if compressible {
            message
                .get("content")
                .and_then(Value::as_str)
                .and_then(|c| serde_json::from_str::<Value>(c).ok())
        } else {
            None
        };

        match parsed {
            Some(value) => {
                let start = Instant::now();
                let compressed = response.compress(&value);
                elapsed += start.elapsed().as_secs_f64() * 1000.0;

                let mut next = message.clone();
                // Serialization of a value that just came from parsing cannot
                // fail; keep the original content if it somehow does rather
                // than dropping the message.
                if let Ok(text) = serde_json::to_string(&compressed) {
                    next.insert("content".to_string(), Value::String(text));
                    messages_compressed += 1;
                }
                out_messages.push(next);
            }
            None => out_messages.push(message.clone()),
        }
    }

    let mut tools_compressed = 0usize;
    let out_tools = scenario.tools.as_ref().map(|tools| {
        tools
            .iter()
            .map(|tool| {
                let start = Instant::now();
                let compressed = schema.compress(tool);
                elapsed += start.elapsed().as_secs_f64() * 1000.0;
                tools_compressed += 1;
                compressed
            })
            .collect()
    });

    TokenlessResult {
        before: conversation_tokens(&scenario.messages, scenario.tools.as_deref(), tk),
        after: conversation_tokens(&out_messages, out_tools.as_deref(), tk),
        messages: out_messages,
        tools: out_tools,
        compress_ms: elapsed,
        messages_compressed,
        tools_compressed,
    }
}

/// Tokens of a whole request: every message, plus the tools array when present.
///
/// The tools array counts because it is sent to the model on every turn, so
/// leaving it out would hide schema compaction entirely.
pub fn conversation_tokens(
    messages: &[Message],
    tools: Option<&[Value]>,
    tk: &Tokenizers,
) -> TokenCount {
    let msgs: TokenCount = messages
        .iter()
        .map(|m| tk.count_value(&Value::Object(m.clone())))
        .sum();
    let tools: TokenCount = tools
        .map(|t| t.iter().map(|v| tk.count_value(v)).sum())
        .unwrap_or_default();
    msgs + tools
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::l3::Suite;
    use crate::l3::asset::AssetSource;

    fn scenario(messages: Vec<Message>, tools: Option<Vec<Value>>) -> Scenario {
        Scenario {
            suite: Suite::Scenario,
            scenario: "t".into(),
            display_name: None,
            content_type: "json".into(),
            size_label: None,
            source: AssetSource {
                reference: "test".into(),
                headroom_native: true,
                headroom_revision: None,
                headroom_dirty: None,
            },
            headroom_target_ms: None,
            model_limit: 200_000,
            messages,
            tools,
        }
    }

    fn msg(role: &str, content: &str) -> Message {
        let mut m = Message::new();
        m.insert("role".into(), Value::String(role.into()));
        m.insert("content".into(), Value::String(content.into()));
        m
    }

    /// An array long enough to cross the default array-truncation threshold.
    fn big_array() -> String {
        let items: Vec<Value> = (0..200)
            .map(|i| serde_json::json!({"id": i, "name": format!("item {i}"), "note": "x".repeat(64)}))
            .collect();
        serde_json::to_string(&items).expect("array serializes")
    }

    #[test]
    fn compresses_json_tool_message() {
        let tk = Tokenizers::load().expect("tokenizers load");
        let s = scenario(vec![msg("tool", &big_array())], None);
        let r = run(&s, &tk);
        assert_eq!(r.messages_compressed, 1);
        assert!(
            r.after.o200k < r.before.o200k,
            "expected reduction, before={} after={}",
            r.before.o200k,
            r.after.o200k
        );
    }

    #[test]
    fn passes_through_non_json_tool_message_byte_identical() {
        // The capability gap: a tool message carrying prose must come back
        // untouched, so the report can distinguish "no entry point" from "tried
        // and achieved nothing".
        let tk = Tokenizers::load().expect("tokenizers load");
        let s = scenario(vec![msg("tool", "The API supports rate limiting.")], None);
        let r = run(&s, &tk);
        assert_eq!(r.messages_compressed, 0);
        assert_eq!(r.before, r.after);
        assert_eq!(r.messages, s.messages);
    }

    #[test]
    fn leaves_user_and_system_prose_untouched() {
        let tk = Tokenizers::load().expect("tokenizers load");
        let s = scenario(
            vec![
                msg("system", "You are a helpful assistant."),
                msg("user", &"a long document ".repeat(200)),
            ],
            None,
        );
        let r = run(&s, &tk);
        assert_eq!(r.messages_compressed, 0);
        assert_eq!(r.before, r.after);
    }

    #[test]
    fn compacts_tool_schemas() {
        let tk = Tokenizers::load().expect("tokenizers load");
        let tool = serde_json::json!({
            "type": "function",
            "function": {
                "name": "search_code",
                "description": "Search code. ".repeat(80),
                "parameters": {
                    "type": "object",
                    "title": "search_code parameters",
                    "properties": {
                        "pattern": {
                            "type": "string",
                            "title": "Pattern",
                            "description": "The pattern. ".repeat(40),
                            "examples": ["a", "b"],
                        }
                    },
                },
            },
        });
        let s = scenario(vec![msg("user", "find it")], Some(vec![tool]));
        let r = run(&s, &tk);
        assert_eq!(r.tools_compressed, 1);
        assert!(
            r.after.o200k < r.before.o200k,
            "expected schema reduction, before={} after={}",
            r.before.o200k,
            r.after.o200k
        );
    }

    #[test]
    fn tools_array_counts_toward_conversation_tokens() {
        // Schema compaction is invisible unless the tools array is counted.
        let tk = Tokenizers::load().expect("tokenizers load");
        let messages = vec![msg("user", "hi")];
        let without = conversation_tokens(&messages, None, &tk);
        let with = conversation_tokens(
            &messages,
            Some(&[serde_json::json!({"type": "function", "function": {"name": "x"}})]),
            &tk,
        );
        assert!(with.o200k > without.o200k);
    }
}
