//! Manual tool-token counts retain identities through captured HTTP/SSE.
#![cfg(target_os = "linux")]
mod common;

use agentsight::aggregator::Aggregator;
use agentsight::analyzer::Analyzer;
use agentsight::event::Event;
use agentsight::parser::Parser;
use agentsight::tokenizer::LlmTokenizer;
use serde_json::{Value, json};

struct Fixture {
    dir: std::path::PathBuf,
    analyzer: Analyzer,
}

impl Fixture {
    fn new() -> Self {
        let dir = common::temp_dir("manual-tools");
        let tokenizer_path = dir.join("tokenizer.json");
        let config_path = dir.join("tokenizer_config.json");
        std::fs::write(
            &tokenizer_path,
            r#"{
            "version":"1.0", "truncation":null, "padding":null,
            "added_tokens":[], "normalizer":null,
            "pre_tokenizer":{"type":"Whitespace"},
            "post_processor":null, "decoder":null,
            "model":{"type":"WordLevel","vocab":{"[UNK]":0},"unk_token":"[UNK]"}
        }"#,
        )
        .unwrap();
        std::fs::write(&config_path, r#"{
            "tokenizer_class":"PreTrainedTokenizerFast",
            "chat_template":"{% for message in messages %}{{ message['role'] + '\n' + message['content'] + '\n' }}{% endfor %}",
            "bos_token":null,"eos_token":null,"unk_token":"[UNK]","model_max_length":32768
        }"#).unwrap();
        let load = || LlmTokenizer::from_file(&tokenizer_path, &config_path).unwrap();
        Self {
            dir,
            analyzer: Analyzer::with_tokenizer(load(), load()),
        }
    }

    fn count(&self, responses: bool, chunks: &[Value]) -> (usize, Vec<usize>) {
        let path = if responses {
            "/v1/responses"
        } else {
            "/v1/chat/completions"
        };
        let body = if responses {
            json!({"model":"fixture-model","input":"hello","stream":true})
        } else {
            json!({"model":"fixture-model","messages":[{"role":"user","content":"hello"}],"stream":true})
        }.to_string();
        let request = format!(
            "POST {path} HTTP/1.1\r\nHost: api.openai.com\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
            body.len()
        );
        let parser = Parser::new();
        let mut aggregator = Aggregator::new();
        assert!(
            aggregator
                .process_result(parser.parse_event(Event::Ssl(common::make_ssl_event(
                    123,
                    0x123,
                    1,
                    request.into_bytes(),
                    "fixture"
                ))))
                .is_empty()
        );
        let mut records = vec![common::make_openai_sse_response_headers()];
        records.extend(chunks.iter().map(|v| format!("data: {v}\n\n").into_bytes()));
        records.push(common::make_sse_done());
        let mut completed = Vec::new();
        for record in records {
            completed.extend(aggregator.process_result(parser.parse_event(Event::Ssl(
                common::make_ssl_event(123, 0x123, 0, record, "fixture"),
            ))));
        }
        let breakdown = completed
            .iter()
            .find_map(|r| self.analyzer.analyze_token_consumption(r))
            .expect("captured SSE must reach the manual counter");
        let blocks = breakdown
            .output_per_block
            .into_iter()
            .filter(|b| b.content_type == "tool_calls")
            .map(|b| b.tokens)
            .collect();
        (breakdown.total_output_tokens, blocks)
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn added(responses: bool, index: u64) -> Value {
    if responses {
        json!({"type":"response.output_item.added","output_index":index,"item":{"type":"function_call","id":format!("fc_{index}"),"call_id":format!("call_{index}"),"name":"lookup","arguments":""}})
    } else {
        json!({"id":"fixture","choices":[{"index":0,"delta":{"role":"assistant","tool_calls":[{"index":index,"id":format!("call_{index}"),"type":"function","function":{"name":"lookup","arguments":""}}]}}]})
    }
}

fn delta(responses: bool, index: u64, text: &str) -> Value {
    if responses {
        json!({"type":"response.function_call_arguments.delta","item_id":format!("fc_{index}"),"output_index":index,"delta":text})
    } else {
        json!({"id":"fixture","choices":[{"index":0,"delta":{"tool_calls":[{"index":index,"function":{"arguments":text}}]}}]})
    }
}

fn check_parallel(responses: bool, interleaved: bool) {
    let fixture = Fixture::new();
    let single = fixture.count(
        responses,
        &[added(responses, 0), delta(responses, 0, "{\"x\":1}")],
    );
    assert!(single.0 > 0);
    assert_eq!(single.1.len(), 1);
    let chunks = if interleaved {
        vec![
            added(responses, 0),
            added(responses, 1),
            delta(responses, 0, "{\"x\":"),
            delta(responses, 1, "{\"x\":"),
            delta(responses, 0, "1}"),
            delta(responses, 1, "1}"),
        ]
    } else {
        vec![
            added(responses, 0),
            delta(responses, 0, "{\"x\":1}"),
            added(responses, 1),
            delta(responses, 1, "{\"x\":1}"),
        ]
    };
    assert_eq!(
        fixture.count(responses, &chunks),
        (2 * single.0, vec![single.1[0]; 2]),
        "each identical call must render its own complete template"
    );
}

#[test]
fn chat_interleaved_calls_keep_their_arguments() {
    check_parallel(false, true);
}
#[test]
fn responses_interleaved_calls_keep_their_arguments() {
    check_parallel(true, true);
}
#[test]
fn chat_sequential_control() {
    check_parallel(false, false);
}
#[test]
fn responses_sequential_control() {
    check_parallel(true, false);
}

#[test]
fn responses_done_replaces_partial_arguments_without_double_counting() {
    let fixture = Fixture::new();
    let expected = fixture.count(true, &[added(true, 0), delta(true, 0, "{\"x\":1}")]);
    let chunks = vec![
        added(true, 0),
        delta(true, 0, "{\"x\":"),
        json!({"type":"response.function_call_arguments.done","item_id":"fc_0","output_index":0,"arguments":"{\"x\":1}"}),
        json!({"type":"response.output_item.done","output_index":0,"item":{"type":"function_call","id":"fc_0","call_id":"call_0","name":"lookup","arguments":"{\"x\":1}"}}),
    ];
    assert_eq!(fixture.count(true, &chunks), expected);
}

#[test]
fn chat_choice_identity_does_not_merge_reused_tool_indexes() {
    let fixture = Fixture::new();
    let single = fixture.count(false, &[added(false, 0), delta(false, 0, "{\"x\":1}")]);
    let mut second_add = added(false, 0);
    second_add["choices"][0]["index"] = json!(1);
    let mut second_delta = delta(false, 0, "{\"x\":1}");
    second_delta["choices"][0]["index"] = json!(1);
    let chunks = vec![
        added(false, 0),
        second_add,
        delta(false, 0, "{\"x\":1}"),
        second_delta,
    ];
    assert_eq!(
        fixture.count(false, &chunks),
        (2 * single.0, vec![single.1[0]; 2])
    );
}

#[test]
fn chat_unindexed_sequential_control() {
    let fixture = Fixture::new();
    let single = fixture.count(false, &[added(false, 0), delta(false, 0, "{\"x\":1}")]);
    let mut chunks = vec![
        added(false, 0),
        delta(false, 0, "{\"x\":1}"),
        added(false, 1),
        delta(false, 1, "{\"x\":1}"),
    ];
    for chunk in &mut chunks {
        chunk["choices"][0]["delta"]["tool_calls"][0]
            .as_object_mut()
            .unwrap()
            .remove("index");
    }
    assert_eq!(
        fixture.count(false, &chunks),
        (2 * single.0, vec![single.1[0]; 2])
    );
}

#[test]
fn chat_late_fragment_still_counts_captured_arguments() {
    let fixture = Fixture::new();
    let count = fixture.count(false, &[delta(false, 0, "{\"x\":1}")]);
    assert!(count.0 > 0);
    assert_eq!(count.1.len(), 1);
}

#[test]
fn responses_late_done_item_recovers_one_complete_call() {
    let fixture = Fixture::new();
    let expected = fixture.count(true, &[added(true, 0), delta(true, 0, "{\"x\":1}")]);
    let item = json!({"type":"response.output_item.done","output_index":0,"item":{"type":"function_call","id":"fc_0","call_id":"call_0","name":"lookup","arguments":"{\"x\":1}"}});
    assert_eq!(fixture.count(true, &[item]), expected);
}
