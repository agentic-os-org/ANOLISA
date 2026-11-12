//! Built-in tool prompts are a separate Gemini input counter.
#![cfg(target_os = "linux")]

use agentsight::analyzer::token::{LLMProvider, TokenParser};
use serde_json::{Value, json};

fn usage(snake: bool, tools: Option<u64>) -> Value {
    let fields = if snake {
        [
            "prompt_token_count",
            "candidates_token_count",
            "thoughts_token_count",
            "tool_use_prompt_token_count",
            "total_token_count",
        ]
    } else {
        [
            "promptTokenCount",
            "candidatesTokenCount",
            "thoughtsTokenCount",
            "toolUsePromptTokenCount",
            "totalTokenCount",
        ]
    };
    let mut usage = json!({});
    for (field, count) in [
        (fields[0], 100),
        (fields[1], 5),
        (fields[2], 15),
        (fields[4], 120 + tools.unwrap_or(0)),
    ] {
        usage[field] = json!(count);
    }
    if let Some(tools) = tools {
        usage[fields[3]] = json!(tools);
    }
    json!({"usageMetadata":usage,"modelVersion":"gemini-2.5-flash"})
}

#[test]
fn complete_usage_includes_tool_prompts_without_changing_output() {
    for snake in [false, true] {
        let parsed = TokenParser::new()
            .parse_json(&usage(snake, Some(40)))
            .unwrap();
        assert_eq!(parsed.input_tokens, 140);
        assert_eq!(parsed.output_tokens, 20);
        assert_eq!(parsed.total_tokens(), 160);
        assert_eq!(parsed.provider, LLMProvider::Gemini);
    }
}

#[test]
fn partial_usage_scan_agrees_with_complete_usage() {
    for snake in [false, true] {
        let full = usage(snake, Some(40)).to_string();
        // A continuation ending before the outer close still contains complete counters.
        let partial = &full[..full.len() - 1];
        let parsed = TokenParser::new().parse_data(partial).unwrap();
        assert_eq!(parsed.input_tokens, 140);
        assert_eq!(parsed.output_tokens, 20);
        assert_eq!(parsed.total_tokens(), 160);
    }
}

#[test]
fn absent_and_zero_tool_prompt_counters_keep_existing_usage() {
    for tools in [None, Some(0)] {
        let parsed = TokenParser::new().parse_json(&usage(false, tools)).unwrap();
        assert_eq!(parsed.input_tokens, 100);
        assert_eq!(parsed.output_tokens, 20);
    }
}

#[path = "common/gemini_usage_fixture.rs"]
mod gemini_usage_fixture;
#[test]
fn native_capture_persists_reconciled_semantic_usage() {
    let call = gemini_usage_fixture::native_call();
    let usage = call.token_usage.unwrap();
    assert_eq!(usage.input_tokens, 140);
    assert_eq!(usage.output_tokens, 20);
    assert_eq!(usage.input_tokens + usage.output_tokens, 160);
}
