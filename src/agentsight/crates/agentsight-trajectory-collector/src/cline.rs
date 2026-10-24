//! Cline task files → ATIF v1.7 converter.
//!
//! Cline (VS Code extension `saoudrizwan.claude-dev`) persists each task under
//! `<UserDataDir>/User/globalStorage/saoudrizwan.claude-dev/tasks/<task-id>/`:
//! `api_conversation_history.json` holds the raw Anthropic `MessageParam[]`
//! model transcript (string/array content, `tool_use`/`tool_result` blocks —
//! but no timestamps and no usage), while `ui_messages.json` holds the UI
//! record stream (`{ts, type: "say"|"ask", ...}`) whose `api_req_started`
//! records carry per-request usage. The api file is the discovered session
//! unit; [`append_ui_records`] merges the sibling ui stream so conversion
//! recovers timestamps and token/cost totals, then the api records are wrapped
//! into the Claude-style envelope and delegated to the shared converter —
//! Cline's transcript already IS the Anthropic message shape.

use std::collections::{HashMap, VecDeque};
use std::path::Path;

use anyhow::Result;

use agentsight_atif::{AtifTrajectory, FinalMetrics};

/// Per-request usage recovered from an `api_req_started` ui record.
#[derive(Debug, Clone, Copy)]
struct RequestUsage {
    ts_ms: Option<i64>,
    prompt: u64,
    completion: u64,
    cache_read: u64,
    cache_write: u64,
}

/// Return `true` when the events look like a Cline task: an api-history
/// record (`{role, content}` with no Claude-style envelope) or a ui record
/// (`{type: say|ask, say|ask}`) — shapes the Claude/Gemini/Codex streams,
/// which all carry a `type` envelope or an `id`, never hit.
pub fn is_cline_task(events: &[serde_json::Value]) -> bool {
    events.iter().any(|e| is_api_record(e) || is_ui_record(e))
}

/// An `api_conversation_history.json` record: a bare Anthropic `MessageParam`
/// (`{role, content}`) without the `{type, message}` envelope Claude-style
/// JSONL uses.
fn is_api_record(event: &serde_json::Value) -> bool {
    let Some(obj) = event.as_object() else {
        return false;
    };
    matches!(
        obj.get("role").and_then(|v| v.as_str()),
        Some("user") | Some("assistant")
    ) && obj.contains_key("content")
        && !obj.contains_key("type")
        && !obj.contains_key("message")
}

/// A `ui_messages.json` record: `{ts, type: "say"|"ask", say?/ask?, text?}`.
fn is_ui_record(event: &serde_json::Value) -> bool {
    let Some(obj) = event.as_object() else {
        return false;
    };
    matches!(
        obj.get("type").and_then(|v| v.as_str()),
        Some("say") | Some("ask")
    ) && (obj.contains_key("say") || obj.contains_key("ask"))
}

/// Parse a whole-file JSON record array (Cline writes its task files with
/// `JSON.stringify(..., null, 2)`, so no single line parses as JSONL).
///
/// Returns `None` unless the document is a non-empty array holding at least
/// one recognizable record, so pretty-printed foreign JSON is never absorbed.
pub fn record_array(content: &str) -> Option<Vec<serde_json::Value>> {
    let content = content.strip_prefix('\u{feff}').unwrap_or(content).trim();
    let doc = serde_json::from_str::<serde_json::Value>(content).ok()?;
    let arr = doc.as_array()?;
    (!arr.is_empty() && arr.iter().any(|e| is_api_record(e) || is_ui_record(e)))
        .then(|| arr.clone())
}

/// Unwrap a whole record array that arrived as one compact JSONL line —
/// `load_jsonl_events` parses that single line into one array event, which
/// would hide the records from per-record detection.
pub fn normalize_events(events: &mut Vec<serde_json::Value>) {
    if events.len() == 1 {
        if let Some(arr) = events[0].as_array() {
            if arr.iter().any(|e| is_api_record(e) || is_ui_record(e)) {
                *events = arr.clone();
            }
        }
    }
}

/// Append the sibling `ui_messages.json` records to the event list.
///
/// The store keys refresh detection on the api file's size/mtime; the ui
/// stream is re-read on every ingest, and every appended turn also rewrites
/// the api file, so the pair stays in sync without extra bookkeeping. A
/// missing or unparsable sibling leaves the events unchanged.
pub fn append_ui_records(session_path: &Path, events: &mut Vec<serde_json::Value>) {
    // Guard against ingesting the ui stream twice when the discovered file
    // is itself `ui_messages.json` (a scan_dirs misconfiguration).
    if session_path
        .file_name()
        .is_some_and(|n| n == "ui_messages.json")
    {
        return;
    }
    let Some(dir) = session_path.parent() else {
        return;
    };
    let Ok(content) = std::fs::read_to_string(dir.join("ui_messages.json")) else {
        log::debug!(
            "Cline ui messages missing next to {}",
            session_path.display()
        );
        return;
    };
    if let Some(records) = record_array(&content) {
        events.extend(records);
    }
}

/// Convert Cline task events into an ATIF v1.7 trajectory.
///
/// Api records become Claude-style events (Cline persists raw Anthropic
/// `MessageParam`s) and share the converter with Qoder/Claude Code. Ui
/// records contribute timestamps — via `conversationHistoryIndex` when
/// present, else the surrounding request's ts — and per-assistant-record
/// usage from `api_req_started`. Usage pairing is sequential, so an
/// interruption annotation record may absorb the next request's metrics;
/// the trajectory totals always sum every request exactly once.
pub fn convert_cline_events(
    events: &[serde_json::Value],
    agent_name: &str,
) -> Result<AtifTrajectory> {
    let mut requests: VecDeque<RequestUsage> = VecDeque::new();
    let mut ts_by_api_index: HashMap<u64, i64> = HashMap::new();
    let mut cost_sum = 0.0f64;
    let mut cost_seen = false;
    let mut usage_seen = false;
    // Totals are accumulated over every usage-bearing request — not over the
    // per-step metrics — so a ui stream momentarily ahead of the api file
    // (live task, response not yet persisted) still counts every request.
    let mut totals = (0u64, 0u64, 0u64, 0u64); // prompt, completion, cache_read, cache_write

    for event in events.iter().filter(|e| is_ui_record(e)) {
        if let (Some(idx), Some(ts)) = (
            event
                .get("conversationHistoryIndex")
                .and_then(|v| v.as_u64()),
            event.get("ts").and_then(|v| v.as_i64()),
        ) {
            ts_by_api_index.entry(idx).or_insert(ts);
        }
        let is_req_start = event.get("say").and_then(|v| v.as_str()) == Some("api_req_started");
        if !is_req_start {
            continue;
        }
        let text = event.get("text").and_then(|v| v.as_str()).unwrap_or("");
        let Ok(usage) = serde_json::from_str::<serde_json::Value>(text) else {
            continue;
        };
        let prompt = usage.get("tokensIn").and_then(|v| v.as_u64());
        let completion = usage.get("tokensOut").and_then(|v| v.as_u64());
        if prompt.is_none() && completion.is_none() {
            // Placeholders and failed requests carry no usage.
            continue;
        }
        if let Some(cost) = usage.get("cost").and_then(|v| v.as_f64()) {
            cost_sum += cost;
            cost_seen = true;
        }
        let req = RequestUsage {
            ts_ms: event.get("ts").and_then(|v| v.as_i64()),
            prompt: prompt.unwrap_or(0),
            completion: completion.unwrap_or(0),
            cache_read: usage
                .get("cacheReads")
                .and_then(|v| v.as_u64())
                .unwrap_or(0),
            cache_write: usage
                .get("cacheWrites")
                .and_then(|v| v.as_u64())
                .unwrap_or(0),
        };
        totals.0 += req.prompt;
        totals.1 += req.completion;
        totals.2 += req.cache_read;
        totals.3 += req.cache_write;
        usage_seen = true;
        requests.push_back(req);
    }

    let api_records: Vec<&serde_json::Value> = events.iter().filter(|e| is_api_record(e)).collect();
    let mut wrapped = Vec::with_capacity(api_records.len());
    for (index, record) in api_records.iter().enumerate() {
        let role = record.get("role").and_then(|v| v.as_str()).unwrap_or("");
        let content = record
            .get("content")
            .cloned()
            .unwrap_or(serde_json::Value::Null);
        // Cline appends an auto-generated <environment_details> text block to
        // every user record; without stripping it, tool-result carriers would
        // look like mixed user turns and leak environment noise into every
        // user step message.
        let content = if role == "user" {
            strip_environment_details(&content)
        } else {
            content
        };

        let mut message = serde_json::json!({"role": role, "content": content});
        if role == "assistant" {
            // Each successful request produces exactly one assistant record,
            // so sequential pairing recovers per-step usage; the consumed
            // request also dates the response when no index mapping exists.
            if let Some(req) = requests.front() {
                if req.prompt > 0 || req.completion > 0 {
                    message["usage"] = serde_json::json!({
                        // Anthropic usage excludes cache from input_tokens;
                        // the shared converter folds the cache fields back in.
                        "input_tokens": req.prompt,
                        "output_tokens": req.completion,
                        "cache_read_input_tokens": req.cache_read,
                        "cache_creation_input_tokens": req.cache_write,
                    });
                }
            }
        }
        let timestamp = ts_by_api_index
            .get(&(index as u64))
            .copied()
            .or_else(|| requests.front().and_then(|req| req.ts_ms))
            .and_then(iso_from_ms);
        if role == "assistant" {
            // Consumed only after both the user turn ahead (which peeked the
            // same request for its timestamp) and this response used it.
            requests.pop_front();
        }
        wrapped.push(serde_json::json!({
            "type": role,
            "timestamp": timestamp,
            "message": message,
        }));
    }

    let mut trajectory = crate::atif::convert_qoder_events(&wrapped, agent_name)?;
    // Totals come from the ui request stream rather than the summed step
    // metrics: a live task's ui stream can be a request ahead of the
    // persisted api file, so every usage-bearing request is counted exactly
    // once regardless of which records the api file already holds.
    if usage_seen || cost_seen {
        let metrics = trajectory
            .final_metrics
            .get_or_insert_with(|| FinalMetrics {
                total_prompt_tokens: Some(0),
                total_completion_tokens: Some(0),
                total_cached_tokens: Some(0),
                total_cost_usd: None,
                total_steps: Some(trajectory.steps.len()),
                extra: None,
            });
        if usage_seen {
            // Match the shared converter's convention: the prompt total folds
            // the cache fields back into Anthropic's cache-exclusive tokensIn.
            metrics.total_prompt_tokens = Some(totals.0 + totals.2 + totals.3);
            metrics.total_completion_tokens = Some(totals.1);
            metrics.total_cached_tokens = Some(totals.2);
        }
        if cost_seen {
            // Cline is the only source that reports per-request cost; round to
            // micro-dollars so float accumulation never shows phantom precision.
            metrics.total_cost_usd = Some((cost_sum * 1e6).round() / 1e6);
        }
    }
    Ok(trajectory)
}

/// Cline-private session info destined for the ATIF `extra` field: replayed
/// message counts plus the fallback project, mirroring
/// `qoder::extract_private_metadata`. Counts describe api records; consecutive
/// assistant records (interruption annotations) merge into one step.
pub fn extract_private_metadata(
    events: &[serde_json::Value],
    fallback_project: &str,
) -> HashMap<String, serde_json::Value> {
    let mut user_count = 0u64;
    let mut assistant_count = 0u64;

    for event in events.iter().filter(|e| is_api_record(e)) {
        let content = event.get("content").unwrap_or(&serde_json::Value::Null);
        match event.get("role").and_then(|v| v.as_str()) {
            Some("user") => {
                if user_yields_step(content) {
                    user_count += 1;
                }
            }
            Some("assistant") => {
                if !content_is_empty(content) {
                    assistant_count += 1;
                }
            }
            _ => {}
        }
    }

    let mut extra = HashMap::new();
    extra.insert(
        "user_message_count".into(),
        serde_json::Value::from(user_count),
    );
    extra.insert(
        "assistant_message_count".into(),
        serde_json::Value::from(assistant_count),
    );
    extra.insert(
        "project".into(),
        serde_json::Value::String(fallback_project.to_string()),
    );
    extra
}

/// A user record that yields a step: genuine text beyond the auto-injected
/// environment block (result-only carriers and injected-only records do not).
fn user_yields_step(content: &serde_json::Value) -> bool {
    match content {
        serde_json::Value::String(s) => !s.trim().is_empty(),
        serde_json::Value::Array(blocks) => blocks.iter().any(|b| {
            b.get("type").and_then(|v| v.as_str()) == Some("text")
                && !is_environment_details(b)
                && b.get("text")
                    .and_then(|v| v.as_str())
                    .is_some_and(|t| !t.trim().is_empty())
        }),
        _ => false,
    }
}

fn content_is_empty(content: &serde_json::Value) -> bool {
    match content {
        serde_json::Value::String(s) => s.trim().is_empty(),
        serde_json::Value::Array(blocks) => blocks.is_empty(),
        serde_json::Value::Null => true,
        _ => false,
    }
}

/// Drop the auto-injected `<environment_details>` text blocks from a user
/// record's content array; every other block (and plain string content)
/// passes through untouched.
fn strip_environment_details(content: &serde_json::Value) -> serde_json::Value {
    let Some(blocks) = content.as_array() else {
        return content.clone();
    };
    serde_json::Value::Array(
        blocks
            .iter()
            .filter(|b| !is_environment_details(b))
            .cloned()
            .collect(),
    )
}

fn is_environment_details(block: &serde_json::Value) -> bool {
    block.get("type").and_then(|v| v.as_str()) == Some("text")
        && block
            .get("text")
            .and_then(|v| v.as_str())
            .is_some_and(|t| t.trim_start().starts_with("<environment_details>"))
}

/// Epoch milliseconds → RFC 3339 with millisecond precision (the string shape
/// every other converter's timestamps already use).
fn iso_from_ms(ms: i64) -> Option<String> {
    chrono::DateTime::from_timestamp_millis(ms)
        .map(|dt| dt.to_rfc3339_opts(chrono::SecondsFormat::Millis, true))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Modeled on a real `tasks/<task-id>/api_conversation_history.json`.
    fn api_content() -> String {
        concat!(
            "[\n",
            "  {\n",
            "    \"role\": \"user\",\n",
            "    \"content\": [\n",
            "      {\n",
            "        \"type\": \"text\",\n",
            "        \"text\": \"Where does Cline store task history?\"\n",
            "      },\n",
            "      {\n",
            "        \"type\": \"text\",\n",
            "        \"text\": \"<environment_details>\\n# VSCode Workspace Files\\n</environment_details>\"\n",
            "      }\n",
            "    ]\n",
            "  },\n",
            "  {\n",
            "    \"role\": \"assistant\",\n",
            "    \"content\": [\n",
            "      {\n",
            "        \"type\": \"text\",\n",
            "        \"text\": \"I'll check the globalStorage directory.\"\n",
            "      },\n",
            "      {\n",
            "        \"type\": \"tool_use\",\n",
            "        \"id\": \"toolu_01\",\n",
            "        \"name\": \"execute_command\",\n",
            "        \"input\": {\n",
            "          \"command\": \"ls\"\n",
            "        }\n",
            "      }\n",
            "    ]\n",
            "  },\n",
            "  {\n",
            "    \"role\": \"user\",\n",
            "    \"content\": [\n",
            "      {\n",
            "        \"type\": \"tool_result\",\n",
            "        \"tool_use_id\": \"toolu_01\",\n",
            "        \"content\": \"tasks\\nsettings\"\n",
            "      },\n",
            "      {\n",
            "        \"type\": \"text\",\n",
            "        \"text\": \"<environment_details>\\n# VSCode Workspace Files\\n</environment_details>\"\n",
            "      }\n",
            "    ]\n",
            "  },\n",
            "  {\n",
            "    \"role\": \"assistant\",\n",
            "    \"content\": [\n",
            "      {\n",
            "        \"type\": \"text\",\n",
            "        \"text\": \"Cline stores tasks under globalStorage/saoudrizwan.claude-dev.\"\n",
            "      }\n",
            "    ]\n",
            "  }\n",
            "]\n",
        )
        .to_string()
    }

    /// Modeled on the matching `ui_messages.json`.
    fn ui_content() -> String {
        concat!(
            "[",
            "{\"ts\":1763071201000,\"type\":\"say\",\"say\":\"task\",\"text\":\"Where does Cline store task history?\",\"conversationHistoryIndex\":0},",
            "{\"ts\":1763071201500,\"type\":\"say\",\"say\":\"api_req_started\",\"text\":\"{\\\"request\\\":\\\"<task>\\\",\\\"tokensIn\\\":120,\\\"tokensOut\\\":80,\\\"cacheWrites\\\":1000,\\\"cacheReads\\\":500,\\\"cost\\\":0.0035}\",\"conversationHistoryIndex\":0},",
            "{\"ts\":1763071210000,\"type\":\"say\",\"say\":\"text\",\"text\":\"I'll check the globalStorage directory.\",\"conversationHistoryIndex\":1},",
            "{\"ts\":1763071212000,\"type\":\"ask\",\"ask\":\"command\",\"text\":\"ls\",\"conversationHistoryIndex\":1},",
            "{\"ts\":1763071220000,\"type\":\"say\",\"say\":\"api_req_started\",\"text\":\"{\\\"request\\\":\\\"ls\\\",\\\"tokensIn\\\":200,\\\"tokensOut\\\":40,\\\"cacheReads\\\":1500,\\\"cost\\\":0.0021}\",\"conversationHistoryIndex\":2},",
            "{\"ts\":1763071230000,\"type\":\"say\",\"say\":\"completion_result\",\"text\":\"Cline stores tasks under globalStorage/saoudrizwan.claude-dev.\",\"conversationHistoryIndex\":3}",
            "]",
        )
        .to_string()
    }

    fn merged_events() -> Vec<serde_json::Value> {
        let mut events = record_array(&api_content()).expect("api fixture parses");
        events.extend(record_array(&ui_content()).expect("ui fixture parses"));
        events
    }

    #[test]
    fn test_is_cline_task() {
        assert!(is_cline_task(&merged_events()));
        let claude = crate::qoder::load_jsonl_events(
            "{\"type\":\"user\",\"timestamp\":\"2026-07-25T10:00:00Z\",\"message\":{\"role\":\"user\",\"content\":\"hi\"}}\n",
        );
        assert!(!is_cline_task(&claude));
        let codex = crate::qoder::load_jsonl_events(
            "{\"timestamp\":\"2026-08-03T09:00:00Z\",\"type\":\"session_meta\",\"payload\":{\"session_id\":\"s-1\"}}\n",
        );
        assert!(!is_cline_task(&codex));
        let gemini = crate::qoder::load_jsonl_events(
            "{\"id\":\"m1\",\"timestamp\":\"2026-10-22T09:00:01Z\",\"type\":\"user\",\"content\":\"q\"}\n",
        );
        assert!(!is_cline_task(&gemini));
    }

    #[test]
    fn test_record_array_rejects_foreign_json() {
        assert_eq!(
            record_array(&api_content()).map(|r| r.len()),
            Some(4),
            "pretty-printed api history parses as a record array"
        );
        assert!(record_array("{\n  \"type\": \"user\"\n}\n").is_none());
        assert!(record_array("[]").is_none());
        assert!(record_array("not json").is_none());
    }

    #[test]
    fn test_convert_basic_flow() {
        let traj = convert_cline_events(&merged_events(), "cline").unwrap();

        assert_eq!(traj.agent.name, "cline");
        traj.validate_step_ids().unwrap();

        // user + agent(tool) + agent(final); the tool-result carrier yields
        // no user step and the environment block never leaks.
        assert_eq!(traj.steps.len(), 3, "steps: {:?}", traj.steps);
        assert_eq!(
            traj.steps[0].message,
            "Where does Cline store task history?"
        );
        assert_eq!(
            traj.steps[0].timestamp.as_deref(),
            Some("2025-11-13T22:00:01.000Z")
        );

        let tool_step = &traj.steps[1];
        assert_eq!(tool_step.message, "I'll check the globalStorage directory.");
        assert_eq!(
            tool_step.timestamp.as_deref(),
            Some("2025-11-13T22:00:10.000Z")
        );
        let calls = tool_step.tool_calls.as_ref().unwrap();
        assert_eq!(calls[0].function_name, "execute_command");
        assert_eq!(calls[0].arguments, serde_json::json!({"command": "ls"}));
        let obs = tool_step.observation.as_ref().unwrap();
        assert_eq!(obs.results[0].source_call_id.as_deref(), Some("toolu_01"));
        assert_eq!(
            obs.results[0].content,
            Some(serde_json::json!("tasks\nsettings"))
        );

        // Usage pairs with the request that produced the record; the shared
        // converter folds cache back into the prompt total.
        let metrics = tool_step.metrics.as_ref().unwrap();
        assert_eq!(metrics.prompt_tokens, Some(120 + 500 + 1000));
        assert_eq!(metrics.completion_tokens, Some(80));
        assert_eq!(metrics.cached_tokens, Some(500));

        // Totals sum every ui request exactly once (R1: 120+500+1000,
        // R2: 200+1500), cache folded into the prompt total.
        let final_metrics = traj.final_metrics.as_ref().unwrap();
        assert_eq!(
            final_metrics.total_prompt_tokens,
            Some(120 + 500 + 1000 + 200 + 1500)
        );
        assert_eq!(final_metrics.total_completion_tokens, Some(120));
        assert_eq!(final_metrics.total_cached_tokens, Some(2000));
        let cost = final_metrics.total_cost_usd.unwrap();
        assert!((cost - 0.0056).abs() < 1e-9, "cost {cost}");
        assert_eq!(final_metrics.total_steps, Some(3));
    }

    #[test]
    fn test_convert_without_ui_records() {
        let events = record_array(&api_content()).unwrap();
        let traj = convert_cline_events(&events, "cline").unwrap();
        assert_eq!(traj.steps.len(), 3);
        assert!(traj.steps.iter().all(|s| s.timestamp.is_none()));
        assert!(traj.steps.iter().all(|s| s.metrics.is_none()));
        assert!(traj.final_metrics.is_none());
    }

    #[test]
    fn test_convert_fallback_timestamps_pair_with_requests() {
        // Older ui streams carry no conversationHistoryIndex: both records of
        // a turn fall back to the ts of the request between them.
        let mut events = record_array(&api_content()).unwrap();
        let ui = record_array(&ui_content())
            .unwrap()
            .into_iter()
            .map(|mut r| {
                if let Some(obj) = r.as_object_mut() {
                    obj.remove("conversationHistoryIndex");
                }
                r
            })
            .collect::<Vec<_>>();
        events.extend(ui);

        let traj = convert_cline_events(&events, "cline").unwrap();
        assert_eq!(
            traj.steps[0].timestamp.as_deref(),
            Some("2025-11-13T22:00:01.500Z")
        );
        assert_eq!(
            traj.steps[1].timestamp.as_deref(),
            Some("2025-11-13T22:00:01.500Z")
        );
        assert_eq!(
            traj.steps[2].timestamp.as_deref(),
            Some("2025-11-13T22:00:20.000Z")
        );
    }

    #[test]
    fn test_extract_private_metadata() {
        let extra = extract_private_metadata(&merged_events(), "myapp");
        assert_eq!(extra["user_message_count"], serde_json::json!(1));
        assert_eq!(extra["assistant_message_count"], serde_json::json!(2));
        assert_eq!(extra["project"], serde_json::json!("myapp"));
    }

    #[test]
    fn test_normalize_events_unwraps_compact_array() {
        // A compact single-line writer puts the whole array on one JSONL line.
        let compact = serde_json::to_string(&record_array(&api_content()).unwrap()).unwrap();
        let mut events = crate::qoder::load_jsonl_events(&compact);
        assert_eq!(events.len(), 1);
        normalize_events(&mut events);
        assert_eq!(events.len(), 4);
        assert!(is_cline_task(&events));

        // Foreign single events stay untouched.
        let mut foreign = crate::qoder::load_jsonl_events(
            "{\"type\":\"user\",\"message\":{\"role\":\"user\",\"content\":\"hi\"}}\n",
        );
        normalize_events(&mut foreign);
        assert_eq!(foreign.len(), 1);
    }

    #[test]
    fn test_append_ui_records() {
        let dir = std::env::temp_dir().join(format!("cline-ui-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let api_path = dir.join("api_conversation_history.json");
        std::fs::write(&api_path, "[]").unwrap();
        std::fs::write(dir.join("ui_messages.json"), ui_content()).unwrap();

        let mut events = record_array(&api_content()).unwrap();
        let before = events.len();
        append_ui_records(&api_path, &mut events);
        assert_eq!(events.len(), before + 6);

        // A task directory without a ui sibling is a no-op.
        let bare = dir.join("bare-task");
        std::fs::create_dir_all(&bare).unwrap();
        let mut events2 = record_array(&api_content()).unwrap();
        append_ui_records(&bare.join("api_conversation_history.json"), &mut events2);
        assert_eq!(events2.len(), before);

        // The ui stream itself is never appended to itself.
        let mut events3 = record_array(&ui_content()).unwrap();
        let n = events3.len();
        append_ui_records(&dir.join("ui_messages.json"), &mut events3);
        assert_eq!(events3.len(), n);

        let _ = std::fs::remove_dir_all(&dir);
    }
}
