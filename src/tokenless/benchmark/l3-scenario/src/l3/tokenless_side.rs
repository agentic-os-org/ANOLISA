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

//! 通过生产 BeforeModel/PostTool 入口测量静态会话，保留实际分派、TOON 与最终仲裁。
//! 无恢复与内存 Stash 两种模式分开，RTK 命令执行和真实恢复调用成本不在本层测量。

use std::sync::Arc;
use std::time::Instant;

use serde_json::Value;
use tokenless_ccr::{InMemoryStore, StashStore};
use tokenless_protocol::{
    Attribution, BeforeModelCapabilities, BeforeModelRequest, BeforeModelResponse, ContentOrigin,
    Disposition, OutputOptimization, PostToolCapabilities, PostToolRequest, PostToolResponse,
    Recoverability, RecoveryMethod, Request, RequestEnvelope, Response, ResultKind,
    ToolResultStatus,
};
use tokenless_runtime::{EntryOptions, RuntimeConfig, dispatch_with_store};

use super::L3Error;
use super::asset::{Message, Scenario};
use super::report::LifecycleSummary;
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
    /// 实际可选择的生产入口；不把无节省与无入口混为一谈。
    pub entry_points: Vec<String>,
    /// 每个 tool 消息的生产分派与仲裁证据。
    pub lifecycle: Vec<PostToolResponse>,
    /// BeforeModel 的实际处置，不丢弃 schema 恢复门禁证据。
    pub before_model: Option<BeforeModelEvidence>,
    /// 每次分派的 stash 与最终仲裁测量证据。
    pub runtime_outcomes: Vec<Value>,
    /// 当前恢复配置不允许选择入口时的原因。
    pub gap_reasons: Vec<String>,
    /// 成功读取的可见 Stash 引用数，不代表 Agent 已付出恢复调用成本。
    pub recovered_references: usize,
    /// 每个可见 Stash 引用及其原文，供带检索工具的语义测试按 key 返回。
    pub stash: Vec<StashEntry>,
}

/// schema 响应及其实际 Runtime 仲裁证据，保持 wire 响应契约不变。
#[derive(Debug, Clone, serde::Serialize)]
pub struct BeforeModelEvidence {
    /// 实际发给宿主的 schema 响应。
    pub response: BeforeModelResponse,
    /// 来自 Runtime 的测量状态。
    pub disposition: Disposition,
    /// 当前输出的恢复约束。
    pub recoverability: Recoverability,
    /// 实际参与测量的算子。
    pub operations: Vec<tokenless_protocol::AppliedOperation>,
}

/// One Stash reference in the emitted conversation and the bytes a retrieve returns.
#[derive(Debug, Clone, serde::Serialize)]
pub struct StashEntry {
    pub key: String,
    pub original: String,
}

impl TokenlessResult {
    /// 汇总各 tool 消息的生产处置；用 wire 名称，与 lifecycle.json 一致。
    pub fn lifecycle_summary(&self) -> LifecycleSummary {
        let recovery_name = |value: Recoverability| match value {
            Recoverability::Lossless => "lossless",
            Recoverability::Retrievable => "retrievable",
            Recoverability::Unrecoverable => "unrecoverable",
        };
        let mut summary = LifecycleSummary {
            stash_entries: self.stash.len(),
            ..LifecycleSummary::default()
        };
        for response in &self.lifecycle {
            summary
                .dispositions
                .push(response.disposition.wire_str().to_owned());
            if response.is_applied() {
                summary
                    .recoverability
                    .push(recovery_name(response.recoverability).to_owned());
            }
            summary.operations.extend(
                response
                    .applied_operations
                    .iter()
                    .map(|operation| operation.wire_str().to_owned()),
            );
        }
        if let Some(response) = &self.before_model {
            summary
                .dispositions
                .push(response.disposition.wire_str().to_owned());
            if response.disposition == Disposition::Applied {
                summary
                    .recoverability
                    .push(recovery_name(response.recoverability).to_owned());
            }
            summary.operations.extend(
                response
                    .operations
                    .iter()
                    .map(|operation| operation.wire_str().to_owned()),
            );
        }
        for list in [
            &mut summary.dispositions,
            &mut summary.recoverability,
            &mut summary.operations,
        ] {
            list.sort();
            list.dedup();
        }
        summary
    }
}

/// Run tokenless over a scenario.
///
/// Messages the compressors have no entry point for are passed through
/// byte-identical, so `before`/`after` stay comparable with the reference side,
/// which receives the same conversation.
///
/// # Errors
/// Runtime 调用或协议形状不符合当前生产契约时返回错误。
pub fn run(scenario: &Scenario, tk: &Tokenizers) -> Result<TokenlessResult, L3Error> {
    run_with_recovery(scenario, tk, false)
}

/// 在明确恢复模式下调用生产 Runtime；无模型、磁盘 Stash 或统计副作用。
///
/// # Errors
/// Runtime、协议形状或可见 Stash 引用校验失败时中止，不发布伪零值。
pub fn run_with_recovery(
    scenario: &Scenario,
    tk: &Tokenizers,
    recovery_available: bool,
) -> Result<TokenlessResult, L3Error> {
    let default = RuntimeConfig::default();
    let options = EntryOptions {
        compression_enabled: default.compression_enabled,
        search_path_sharing_enabled: default.search_path_sharing_enabled,
        diff_compression_enabled: default.diff_compression_enabled,
        html_extraction_enabled: default.html_extraction_enabled,
        mixed_content_enabled: default.mixed_content_enabled,
        stash_enabled: recovery_available,
        rtk_launcher: None,
        rtk_data_dir: None,
    };
    let store: Arc<dyn StashStore> = Arc::new(InMemoryStore::new());
    let attached = recovery_available.then_some(&store);
    let recovery = if recovery_available {
        RecoveryMethod::Shell
    } else {
        RecoveryMethod::None
    };
    let mut elapsed = 0.0_f64;
    let mut messages_compressed = 0usize;
    let mut out_messages = Vec::with_capacity(scenario.messages.len());
    let mut entry_points = Vec::new();
    let mut lifecycle = Vec::new();
    let mut before_model = None;
    let mut runtime_outcomes = Vec::new();
    let mut gap_reasons = Vec::new();
    let mut recovered_references = 0;
    let mut stash = Vec::new();
    for message in &scenario.messages {
        let mut next = message.clone();
        if message.get("role").and_then(Value::as_str) == Some("tool")
            && let Some(content) = message.get("content").and_then(Value::as_str)
        {
            let (name, origin, command) = tool_context(scenario, message);
            let request = PostToolRequest {
                result_kind: ResultKind::Tool,
                tool_name: name,
                content: content.to_owned(),
                status: ToolResultStatus::Success,
                content_origin: origin,
                command,
                output_optimization: OutputOptimization::None,
                capabilities: PostToolCapabilities {
                    replace_output: true,
                    replace_with_text: true,
                    recovery: recovery.clone(),
                },
            };
            let start = Instant::now();
            let outcome = dispatch_with_store(
                &RequestEnvelope {
                    attribution: Attribution::new("l3-static-harness"),
                    request: Request::PostTool(request),
                },
                &options,
                attached,
                None,
            )
            .map_err(|error| L3Error::Runtime(error.to_string()))?;
            elapsed += start.elapsed().as_secs_f64() * 1000.0;
            runtime_outcomes.push(runtime_evidence(&outcome)?);
            let Response::PostTool(response) = outcome.response.response else {
                return Err(L3Error::Runtime(
                    "PostTool returned a different operation".into(),
                ));
            };
            if matches!(
                response.disposition,
                Disposition::Applied | Disposition::NoSavings
            ) && let Some(content_type) = response.content_type
            {
                entry_points.push(format!(
                    "PostTool {} (production capability/recovery gates)",
                    content_type.wire_str()
                ));
            } else if response.disposition == Disposition::RecoverabilityUnavailable {
                gap_reasons.push(
                    "PostTool recovery capability unavailable in the selected mode".to_owned(),
                );
            }
            for key in &response.stash_keys {
                let original = store
                    .retrieve(key)
                    .map_err(|error| L3Error::Runtime(error.to_string()))?;
                let Some(original) = original else {
                    return Err(L3Error::Runtime(format!(
                        "unrecoverable emitted stash reference {key}"
                    )));
                };
                recovered_references += 1;
                stash.push(StashEntry {
                    key: key.clone(),
                    original,
                });
            }
            if response.output != content {
                messages_compressed += 1;
                next.insert("content".into(), Value::String(response.output.clone()));
            }
            lifecycle.push(response);
        }
        out_messages.push(next);
    }
    let mut tools_compressed = 0;
    let out_tools = if let Some(tools) = &scenario.tools {
        let start = Instant::now();
        let outcome = dispatch_with_store(
            &RequestEnvelope {
                attribution: Attribution::new("l3-static-harness"),
                request: Request::BeforeModel(BeforeModelRequest {
                    tools: tools.clone(),
                    visible_context: Value::Array(
                        out_messages.iter().cloned().map(Value::Object).collect(),
                    ),
                    capabilities: BeforeModelCapabilities {
                        replace_tools: true,
                        recovery: if recovery_available {
                            RecoveryMethod::tool("fixture_retrieve")
                                .map_err(|error| L3Error::Runtime(error.to_string()))?
                        } else {
                            RecoveryMethod::None
                        },
                    },
                }),
            },
            &options,
            attached,
            None,
        )
        .map_err(|error| L3Error::Runtime(error.to_string()))?;
        elapsed += start.elapsed().as_secs_f64() * 1000.0;
        runtime_outcomes.push(runtime_evidence(&outcome)?);
        let measurement = outcome
            .stats
            .as_ref()
            .ok_or_else(|| L3Error::Runtime("BeforeModel returned no measurement".to_owned()))?;
        let disposition = measurement.disposition();
        let recoverability = measurement.recoverability();
        let operations = measurement.applied_operations().to_vec();
        for key in &outcome.artifact_keys {
            let original = store
                .retrieve(key)
                .map_err(|error| L3Error::Runtime(error.to_string()))?
                .ok_or_else(|| L3Error::Runtime(format!("unretrievable schema artifact {key}")))?;
            recovered_references += 1;
            stash.push(StashEntry {
                key: key.clone(),
                original,
            });
        }
        let Response::BeforeModel(response) = outcome.response.response else {
            return Err(L3Error::Runtime(
                "BeforeModel returned a different operation".into(),
            ));
        };
        if !tools.is_empty() && matches!(disposition, Disposition::Applied | Disposition::NoSavings)
        {
            entry_points.push("compress-schema (production BeforeModel)".into());
        } else if disposition == Disposition::RecoverabilityUnavailable {
            gap_reasons.push("BeforeModel schema recovery requires the declared retrieval tool; unavailable in this mode".to_owned());
        }
        tools_compressed = tools
            .iter()
            .zip(&response.tools)
            .filter(|(before, after)| before != after)
            .count();
        before_model = Some(BeforeModelEvidence {
            response: response.clone(),
            disposition,
            recoverability,
            operations,
        });
        Some(response.tools)
    } else {
        None
    };
    entry_points.sort();
    entry_points.dedup();
    Ok(TokenlessResult {
        before: conversation_tokens(&scenario.messages, scenario.tools.as_deref(), tk),
        after: conversation_tokens(&out_messages, out_tools.as_deref(), tk),
        messages: out_messages,
        tools: out_tools,
        compress_ms: elapsed,
        messages_compressed,
        tools_compressed,
        entry_points,
        lifecycle,
        before_model,
        runtime_outcomes,
        gap_reasons,
        recovered_references,
        stash,
    })
}

fn runtime_evidence(outcome: &tokenless_runtime::EntryOutcome) -> Result<Value, L3Error> {
    if outcome.stash_errors.is_some_and(|count| count > 0) {
        return Err(L3Error::Runtime(
            "Runtime reported failed stash operations".to_owned(),
        ));
    }
    Ok(serde_json::json!({
        "runtime_final_acceptance_measured": outcome.stats.is_some(),
        "disposition": outcome.stats.as_ref().map(|stats| stats.disposition().wire_str()),
        "recoverability": outcome.stats.as_ref().map(|stats| stats.recoverability()),
        "operations": outcome.stats.as_ref().map(|stats| stats.applied_operations()),
        "stash_writes": outcome.stash_writes, "stash_errors": outcome.stash_errors,
        "stash_size": outcome.stash_size, "artifact_keys": outcome.artifact_keys,
    }))
}

fn tool_context(scenario: &Scenario, message: &Message) -> (String, ContentOrigin, Option<String>) {
    let id = message.get("tool_call_id").and_then(Value::as_str);
    let function = scenario
        .messages
        .iter()
        .filter_map(|entry| entry.get("tool_calls").and_then(Value::as_array))
        .flatten()
        .find(|call| id.is_some() && call.get("id").and_then(Value::as_str) == id)
        .and_then(|call| call.get("function"));
    let name = function
        .and_then(|value| value.get("name"))
        .and_then(Value::as_str)
        .or_else(|| message.get("name").and_then(Value::as_str))
        .unwrap_or("fixture_api");
    let origin = match name {
        "exec" | "Bash" | "Shell" => ContentOrigin::CommandOutput,
        "Read" | "read_file" => ContentOrigin::FileContent,
        _ => ContentOrigin::ApiResponse,
    };
    let command = function
        .and_then(|value| value.get("arguments"))
        .and_then(Value::as_str)
        .and_then(|arguments| serde_json::from_str::<Value>(arguments).ok())
        .and_then(|arguments| {
            arguments
                .get("command")
                .and_then(Value::as_str)
                .map(str::to_owned)
        });
    (name.to_owned(), origin, command)
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
    use tokenless_protocol::ContentType;

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
            critical_facts: Vec::new(),
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
        let r = run(&s, &tk).expect("runtime adapted");
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
        let r = run(&s, &tk).expect("runtime adapted");
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
        let r = run(&s, &tk).expect("runtime adapted");
        assert_eq!(r.messages_compressed, 0);
        assert_eq!(r.before, r.after);
    }

    #[test]
    fn schemas_without_a_retrieval_tool_remain_verbatim() {
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
        let r = run(&s, &tk).expect("runtime adapted");
        assert_eq!(r.tools_compressed, 0);
        assert_eq!(r.tools, s.tools);
        assert_eq!(r.before, r.after);
        assert!(r.entry_points.is_empty());
        assert!(!r.gap_reasons.is_empty());
        let with = run_with_recovery(&s, &tk, true).unwrap();
        assert!(with.tools_compressed > 0);
        assert!(with.after.o200k < with.before.o200k);
        assert!(
            with.lifecycle_summary()
                .operations
                .iter()
                .any(|name| name == "schema_compression")
        );
        assert!(!with.stash.is_empty());
        let store: Arc<dyn StashStore> = Arc::new(InMemoryStore::new());
        let config = RuntimeConfig::default();
        let options = EntryOptions {
            compression_enabled: config.compression_enabled,
            search_path_sharing_enabled: config.search_path_sharing_enabled,
            diff_compression_enabled: config.diff_compression_enabled,
            html_extraction_enabled: config.html_extraction_enabled,
            mixed_content_enabled: config.mixed_content_enabled,
            stash_enabled: true,
            rtk_launcher: None,
            rtk_data_dir: None,
        };
        let outcome = dispatch_with_store(
            &RequestEnvelope {
                attribution: Attribution::new("l3-recovery-contract-test"),
                request: Request::BeforeModel(BeforeModelRequest {
                    tools: s.tools.clone().unwrap(),
                    visible_context: Value::Null,
                    capabilities: BeforeModelCapabilities {
                        replace_tools: true,
                        recovery: RecoveryMethod::tool("fixture_retrieve").unwrap(),
                    },
                }),
            },
            &options,
            Some(&store),
            None,
        )
        .unwrap();
        let Response::BeforeModel(response) = outcome.response.response else {
            panic!("wrong operation")
        };
        assert!(
            conversation_tokens(&s.messages, Some(&response.tools), &tk).o200k < r.before.o200k
        );
        assert!(!outcome.artifact_keys.is_empty());
        for key in outcome.artifact_keys {
            assert!(store.retrieve(&key).unwrap().is_some());
        }
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

    #[test]
    fn current_search_and_table_domains_use_production_dispatch() {
        let tk = Tokenizers::load().unwrap();
        let search = (0..64)
            .map(|index| format!("fixtures/repeated/source.rs:{index}:required_fact_{index}\n"))
            .collect::<String>();
        let table = format!(
            "\"id\",\"message\"\r\n{}",
            (0..64)
                .map(|index| format!("\"{index}\",\"required_fact_{index}\"\r\n"))
                .collect::<String>()
        );
        for (content, expected) in [
            (search, ContentType::SearchResults),
            (table, ContentType::Tabular),
        ] {
            let mut s = scenario(vec![msg("tool", &content)], None);
            s.critical_facts = vec!["required_fact_63".into()];
            let result = run(&s, &tk).unwrap();
            assert_eq!(result.lifecycle[0].content_type, Some(expected));
            assert!(result.lifecycle[0].is_applied());
            for index in 0..64 {
                assert!(
                    result.lifecycle[0]
                        .output
                        .contains(&format!("required_fact_{index}"))
                );
            }
            let facts = crate::l3::retention::critical_items(&s);
            let reverse = crate::l3::retention::check(
                &facts,
                &[msg("tool", "unrelated_header\nno_source_facts\n")],
                None,
            );
            assert!(reverse.passed < reverse.total);
        }
    }

    #[test]
    fn html_recovery_gate_is_not_bypassed_by_the_harness() {
        let tk = Tokenizers::load().unwrap();
        let html = format!(
            "<!doctype html><html><head><style>{}</style></head><body><main><h1>required-section</h1><p>Complete source evidence includes required_fact_42 and all explanatory paragraph details.</p></main></body></html>",
            "a {{ color:red; }}".repeat(100)
        );
        let s = scenario(vec![msg("tool", &html)], None);
        let without = run(&s, &tk).unwrap();
        assert_eq!(without.messages, s.messages);
        let with = run_with_recovery(&s, &tk, true).unwrap();
        assert!(with.lifecycle[0].is_applied());
        assert!(with.lifecycle[0].output.contains("required_fact_42"));
        assert!(!with.lifecycle[0].stash_keys.is_empty());
        assert_eq!(
            with.recovered_references,
            with.lifecycle[0].stash_keys.len()
        );
        let summary = with.lifecycle_summary();
        assert_eq!(summary.dispositions, vec!["applied".to_owned()]);
        assert_eq!(summary.recoverability, vec!["retrievable".to_owned()]);
        assert!(summary.operations.iter().any(|op| op == "html_extraction"));
        assert_eq!(summary.stash_entries, with.stash.len());
        for entry in &with.stash {
            assert!(with.lifecycle[0].output.contains(&entry.key));
            assert!(!entry.original.is_empty() && html.contains(entry.original.as_str()));
        }
        let summary = without.lifecycle_summary();
        assert!(summary.recoverability.is_empty());
        assert_eq!(summary.stash_entries, 0);
    }

    #[test]
    fn command_build_log_recovery_and_authoritative_file_exclusion_match_runtime() {
        let tk = Tokenizers::load().unwrap();
        let log = format!(
            "{}error[E0308]: required_fact_type_mismatch\n",
            (0..100)
                .map(|index| format!("   Compiling dependency_{index} v1.0.0\n"))
                .collect::<String>()
        );
        let mut command = msg("tool", &log);
        command.insert("name".into(), Value::String("exec".into()));
        let s = scenario(vec![command.clone()], None);
        let without = run(&s, &tk).unwrap();
        assert_eq!(without.messages, s.messages);
        let with = run_with_recovery(&s, &tk, true).unwrap();
        assert!(with.lifecycle[0].is_applied());
        assert!(
            with.lifecycle[0]
                .output
                .contains("required_fact_type_mismatch")
        );
        assert!(with.recovered_references > 0);
        command.insert("name".into(), Value::String("Read".into()));
        let file = scenario(vec![command], None);
        let file_result = run_with_recovery(&file, &tk, true).unwrap();
        assert_eq!(file_result.messages, file.messages);
        assert_eq!(file_result.messages_compressed, 0);
    }

    #[test]
    fn applicability_comes_from_runtime_outcomes_for_diff_and_file_read_html() {
        let tk = Tokenizers::load().unwrap();
        let diff = format!(
            "diff --git a/source.rs b/source.rs\n--- a/source.rs\n+++ b/source.rs\n@@ -1,81 +1,81 @@\n{}-let old = 1;\n+let changed = 2;\n",
            (0..80)
                .map(|index| format!(" unchanged_context_{index}\n"))
                .collect::<String>()
        );
        let html = format!(
            "<html><head><style>{}</style></head><body><p>required evidence</p></body></html>",
            "p{{color:red}}".repeat(100)
        );
        for (content, command, expected) in
            [(diff, "git diff", true), (html, "cat index.html", false)]
        {
            let assistant: Message = serde_json::from_value(serde_json::json!({"role":"assistant", "tool_calls":[{"id":"fixture-call", "function":{"name":"exec", "arguments":serde_json::json!({"command":command}).to_string()}}]})).unwrap();
            let mut result = msg("tool", &content);
            result.insert("tool_call_id".into(), Value::String("fixture-call".into()));
            let s = scenario(vec![assistant, result], None);
            let run = run_with_recovery(&s, &tk, true).unwrap();
            assert_eq!(
                !run.entry_points.is_empty(),
                expected,
                "{command}: {:?}",
                run.lifecycle
            );
            if expected {
                assert_eq!(run.lifecycle[0].content_type, Some(ContentType::Diff));
                assert!(run.lifecycle[0].is_applied());
                assert!(run.after.o200k < run.before.o200k);
            } else {
                assert_eq!(run.messages, s.messages);
            }
        }
    }

    #[test]
    fn json_numeric_literals_survive_the_runtime_without_float_rerendering() {
        let tk = Tokenizers::load().unwrap();
        let input = format!(
            "{{\n\"integer\":9007199254740993,\"decimal\":0.12345678901234567890123456789,\"zero\":-0.0,\"padding\":\"{}\"\n}}",
            "value ".repeat(100)
        );
        let s = scenario(vec![msg("tool", &input)], None);
        let result = run(&s, &tk).unwrap();
        for literal in [
            "9007199254740993",
            "0.12345678901234567890123456789",
            "-0.0",
        ] {
            assert!(result.lifecycle[0].output.contains(literal));
        }
    }
}
