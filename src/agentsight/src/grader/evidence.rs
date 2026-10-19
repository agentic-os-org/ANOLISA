//! Evidence reference helpers for grader dimensions and findings.

use super::input::EvaluationInput;
use super::types::{EvaluationRef, EvidenceDeeplink, EvidenceTarget, EvidenceType};
use crate::storage::sqlite::InterruptionRecord;
use crate::storage::sqlite::genai::TraceEventDetail;

pub(super) fn has_usable_output(event: &TraceEventDetail) -> bool {
    if let Some(raw) = event.output_messages.as_deref() {
        if raw_contains_content(raw) {
            return true;
        }
    }

    // Text is not the only kind of output: a turn that only requests tool
    // calls serializes as tool_call parts, which this helper does not
    // recognize. A recorded output-token count still says the model
    // produced something, so it stays as the fallback.
    event.output_tokens > 0
}

pub(super) fn looks_like_tool_failure(event: &TraceEventDetail) -> bool {
    event
        .output_messages
        .as_deref()
        .is_some_and(contains_tool_failure_signal)
        || event
            .input_messages
            .as_deref()
            .is_some_and(contains_structured_tool_failure_signal)
}

fn contains_tool_failure_signal(raw: &str) -> bool {
    if contains_structured_tool_failure_signal(raw) {
        return true;
    }

    // An unparseable payload that still names a tool response carries that
    // response's text verbatim, so the same error signals apply — with the
    // generic words excluded like everywhere else in this file.
    let text = raw.to_ascii_lowercase();
    text.contains("tool_call_response")
        && (text.contains("\"error\"") || text.contains("traceback"))
}

fn contains_structured_tool_failure_signal(raw: &str) -> bool {
    serde_json::from_str::<serde_json::Value>(raw)
        .map(|value| json_has_tool_failure(&value))
        .unwrap_or(false)
}

fn json_has_tool_failure(value: &serde_json::Value) -> bool {
    match value {
        serde_json::Value::Array(items) => items.iter().any(json_has_tool_failure),
        serde_json::Value::Object(map) => {
            // Both the parts shape and the raw wire forms reach this column: a
            // request stored verbatim by the crash drain carries OpenAI's
            // `role: "tool"` message and Responses' `function_call_output`
            // item, and a failure in either used to score as "no deterministic
            // tool failure".
            let kind = map.get("type").and_then(|value| value.as_str());
            let is_tool_response = kind.is_some_and(|kind| {
                matches!(
                    kind,
                    "tool_call_response" | "tool_result" | "function_call_output"
                )
            }) || map.contains_key("tool_call_response")
                || map.get("role").and_then(|value| value.as_str()) == Some("tool")
                    && map.contains_key("tool_call_id");

            if is_tool_response && tool_response_has_error(map) {
                return true;
            }

            map.values().any(json_has_tool_failure)
        }
        _ => false,
    }
}

/// Whether a tool-result payload declares its own outcome, and which one.
///
/// Both spellings of the error flag are in use — `is_error` and the camelCase
/// `isError` that real agent traces carry — and some tools report `success`
/// or an `error` status instead. When a payload declares success, the content
/// is free-form output that may legitimately mention a traceback or a missing
/// path, so it must not be read as a failure; only an `error` status can
/// contradict the flag, and it wins. The interruption detector reads the same
/// payloads with the same precedence in `tool_response_failure_text`.
fn declared_failure(map: &serde_json::Map<String, serde_json::Value>) -> Option<bool> {
    let status_is_error = map
        .get("status")
        .and_then(|value| value.as_str())
        .is_some_and(|status| status.eq_ignore_ascii_case("error"));

    if let Some(is_error) = map
        .get("is_error")
        .or_else(|| map.get("isError"))
        .and_then(|value| value.as_bool())
    {
        return Some(is_error || status_is_error);
    }

    if let Some(success) = map.get("success").and_then(|value| value.as_bool()) {
        return Some(!success || status_is_error);
    }

    status_is_error.then_some(true)
}

fn tool_response_has_error(map: &serde_json::Map<String, serde_json::Value>) -> bool {
    if let Some(is_error) = declared_failure(map) {
        return is_error;
    }

    // The Responses raw wire form carries its payload in `output`, with no
    // status or error flag to settle the question, so the scan reads that
    // key too.
    ["response", "content", "error", "output"]
        .iter()
        .any(|key| map.get(*key).is_some_and(value_has_error_signal))
}

fn value_has_error_signal(value: &serde_json::Value) -> bool {
    match value {
        // A payload that is entirely valid JSON is read through its parsed
        // form. Scanning the encoded text again would re-judge a result that
        // declares success by its free-form content — the object arm already
        // trusts that flag — and could also match words that only appear in a
        // key or an escape. The Responses API copies
        // `function_call_output.output` through unchanged, and an MCP result
        // is often serialized as `{"content":…,"isError":false}` text.
        serde_json::Value::String(text) => serde_json::from_str::<serde_json::Value>(text.trim())
            .ok()
            .map_or_else(
                || text_has_error_signal(text),
                |parsed| value_has_error_signal(&parsed),
            ),
        serde_json::Value::Array(items) => items.iter().any(value_has_error_signal),
        serde_json::Value::Object(map) => match declared_failure(map) {
            Some(is_error) => is_error,
            None => map.values().any(value_has_error_signal),
        },
        _ => false,
    }
}

/// Whether free-form tool text carries an error signal.
///
/// Deliberately excludes generic words like "exception" or "failed" — the
/// detector's twin scan (`text_has_tool_error_signal`) excludes them for the
/// same reason: normal, non-error output routinely contains them ("handled the
/// exception gracefully", "test result: ok. 30 passed; 0 failed"). Reading
/// them here flagged a passing run as a deterministic tool failure, so the
/// `tool_use` dimension scored 0.45 with a `tool_failure` finding while the
/// detector recorded no failure for the same payload.
fn text_has_error_signal(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    lower.contains("traceback")
        || mentions_failing_exit_code(&lower)
        || lower.contains("no such file or directory")
        || lower.contains("permission denied")
        || lower.contains("command not found")
        || lower.contains("eacces")
        || lower.contains("eperm")
        || lower.contains("\"error\"")
        || lower.contains("error:")
}

/// Whether any `exit code N` mention carries a non-zero status.
///
/// The detector's twin scan (`tool_response_failure_text`) reads the same
/// payloads with this same rule: `exit code 0` is the success spelling a
/// passing command's output routinely carries ("60 passing (exit code 0)"),
/// while any non-zero status is the failure signal. The status may be
/// separated from the words by spaces, a colon or an equals sign.
fn mentions_failing_exit_code(lower: &str) -> bool {
    let mut search_from = 0;
    while let Some(found) = lower[search_from..].find("exit code") {
        let after = &lower[search_from + found + "exit code".len()..];
        let status = after.trim_start_matches([' ', ':', '=']);
        if status.starts_with(|c: char| c.is_ascii_digit() && c != '0') {
            return true;
        }
        search_from += found + "exit code".len();
    }
    false
}

pub(super) fn first_event_refs(input: &EvaluationInput, label: &str) -> Vec<EvaluationRef> {
    input
        .events
        .first()
        .map(|event| vec![genai_ref(&input.target_id, event, label)])
        .unwrap_or_default()
}

pub(super) fn genai_ref(
    conversation_id: &str,
    event: &TraceEventDetail,
    label: &str,
) -> EvaluationRef {
    let id = event
        .call_id
        .clone()
        .unwrap_or_else(|| format!("genai-event-{}", event.id));
    EvaluationRef {
        evidence_type: EvidenceType::GenaiEvent,
        id,
        label: label.to_string(),
        severity: event.interruption_type.clone(),
        target: EvidenceTarget {
            conversation_id: conversation_id.to_string(),
            trace_id: event.trace_id.clone(),
            call_id: event.call_id.clone(),
            step_id: None,
        },
        deeplink: Some(EvidenceDeeplink {
            route: "/atif".to_string(),
            query: serde_json::json!({
                "type": "conversation",
                "id": conversation_id,
                "highlight_call_id": &event.call_id,
            }),
        }),
        metadata: serde_json::json!({
            "event_id": event.id,
            "model": &event.model,
            "status": &event.status,
        }),
    }
}

pub(super) fn interruption_ref(
    conversation_id: &str,
    record: &InterruptionRecord,
) -> EvaluationRef {
    EvaluationRef {
        evidence_type: EvidenceType::Interruption,
        id: record.interruption_id.clone(),
        label: record.interruption_type.clone(),
        severity: Some(record.severity.clone()),
        target: EvidenceTarget {
            conversation_id: conversation_id.to_string(),
            trace_id: record.trace_id.clone(),
            call_id: record.call_id.clone(),
            step_id: None,
        },
        deeplink: Some(EvidenceDeeplink {
            route: "/atif".to_string(),
            query: serde_json::json!({
                "type": "conversation",
                "id": conversation_id,
                "highlight_call_id": &record.call_id,
                "interruption_id": &record.interruption_id,
            }),
        }),
        metadata: serde_json::json!({
            "occurred_at_ns": record.occurred_at_ns,
            "detail": &record.detail,
            "resolved": record.resolved,
        }),
    }
}

fn raw_contains_content(raw: &str) -> bool {
    let trimmed = raw.trim();
    if trimmed.is_empty() || trimmed == "[]" || trimmed == "{}" {
        return false;
    }
    serde_json::from_str::<serde_json::Value>(trimmed)
        .map(|value| json_has_text(&value))
        .unwrap_or_else(|_| !trimmed.is_empty())
}

fn json_has_text(value: &serde_json::Value) -> bool {
    match value {
        serde_json::Value::String(text) => !text.trim().is_empty(),
        serde_json::Value::Array(values) => values.iter().any(json_has_text),
        serde_json::Value::Object(map) => map.iter().any(|(key, value)| {
            matches!(
                key.as_str(),
                "content" | "text" | "message" | "output" | "response"
            ) && json_has_text(value)
                || key == "parts" && json_has_text(value)
                || key == "Text" && json_has_text(value)
                || key == "Reasoning" && json_has_text(value)
        }),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn event(
        input_messages: Option<&str>,
        output_messages: Option<&str>,
        event_json: Option<&str>,
    ) -> TraceEventDetail {
        TraceEventDetail {
            id: 1,
            call_id: Some("call-1".to_string()),
            start_timestamp_ns: 100,
            end_timestamp_ns: Some(200),
            model: Some("test-model".to_string()),
            input_tokens: 10,
            output_tokens: 10,
            total_tokens: 20,
            input_messages: input_messages.map(str::to_string),
            output_messages: output_messages.map(str::to_string),
            system_instructions: None,
            agent_name: Some("agent".to_string()),
            process_name: None,
            pid: Some(1),
            user_query: Some("do work".to_string()),
            event_json: event_json.map(str::to_string),
            trace_id: Some("trace-1".to_string()),
            conversation_id: Some("conv-1".to_string()),
            cache_read_tokens: None,
            status: Some("complete".to_string()),
            interruption_type: None,
        }
    }

    #[test]
    fn ignores_historical_tool_failure_text_in_raw_event_json() {
        let event = event(
            Some(r#"[{"role":"user","content":"write a Linux troubleshooting guide"}]"#),
            Some(r#"[{"role":"assistant","content":"step 1: check the process"}]"#),
            Some(
                r#"{"request":{"messages":[{"role":"user","content":"tool_call_response: {\"error\":\"failed\", \"traceback\":\"FileNotFoundError\"}"}]},"response":{"messages":[{"role":"assistant","content":"step 1: check the process"}]},"error":null}"#,
            ),
        );

        assert!(!looks_like_tool_failure(&event));
    }

    #[test]
    fn detects_tool_failure_in_current_assistant_output() {
        let event = event(
            Some(r#"[{"role":"user","content":"run the tool"}]"#),
            Some(
                r#"[{"role":"assistant","content":"tool_call_response: {\"error\":\"failed to read config\", \"traceback\":\"FileNotFoundError\"}"}]"#,
            ),
            None,
        );

        assert!(looks_like_tool_failure(&event));
    }

    #[test]
    fn tool_call_only_output_counts_as_usable() {
        // The model's turn consists of tool calls; there is no text part to
        // find, but the call produced output tokens.
        let event = event(
            Some(r#"[{"role":"user","content":"list the files"}]"#),
            Some(
                r#"[{"role":"assistant","parts":[{"tool_call":{"id":"c1","name":"list_dir","arguments":"{}"}}]}]"#,
            ),
            None,
        );

        assert!(has_usable_output(&event));
    }

    #[test]
    fn empty_output_without_tokens_is_not_usable() {
        let mut event = event(
            Some(r#"[{"role":"user","content":"list the files"}]"#),
            Some("[]"),
            None,
        );
        event.output_tokens = 0;

        assert!(!has_usable_output(&event));
    }

    #[test]
    fn detects_tool_failure_in_structured_input_tool_result() {
        let event = event(
            Some(
                r#"[{"role":"user","parts":[{"type":"tool_call_response","id":"toolu_1","response":{"content":"Exit code 1\ncat: /tmp/missing.txt: No such file or directory","is_error":true}}]}]"#,
            ),
            Some(r#"[{"role":"assistant","parts":[{"type":"text","content":"file missing"}]}]"#),
            None,
        );

        assert!(looks_like_tool_failure(&event));
    }

    #[test]
    fn ignores_structured_tool_result_with_explicit_non_error_flag() {
        let event = event(
            Some(
                r#"[{"role":"user","parts":[{"type":"tool_call_response","id":"toolu_1","response":{"content":"5 passed, 2 failed","is_error":false}}]}]"#,
            ),
            Some(r#"[{"role":"assistant","parts":[{"type":"text","content":"tests completed"}]}]"#),
            None,
        );

        assert!(!looks_like_tool_failure(&event));
    }

    #[test]
    fn detects_tool_failure_from_nested_is_error_flag() {
        let event = event(
            Some(
                r#"[{"role":"user","parts":[{"type":"tool_call_response","id":"toolu_1","response":{"is_error":true}}]}]"#,
            ),
            Some(r#"[{"role":"assistant","parts":[{"type":"text","content":"tool failed"}]}]"#),
            None,
        );

        assert!(looks_like_tool_failure(&event));
    }

    #[test]
    fn respects_every_spelling_of_a_declared_outcome() {
        // `isError` is the camelCase spelling real agent traces carry (the
        // interruption detector reads both spellings), and some tools report
        // `success` instead. A successful result's content is free-form
        // output: a search that skips unreadable directories prints
        // "Permission denied" and still exits 0.
        for payload in [
            r#"[{"type":"tool_result","isError":false,"content":"find: '/root': Permission denied"}]"#,
            r#"[{"type":"tool_result","success":true,"content":"find: '/root': Permission denied"}]"#,
        ] {
            let event = event(None, Some(payload), None);
            assert!(
                !looks_like_tool_failure(&event),
                "a tool result that declares success is not a failure: {payload}"
            );
        }

        // The failure flag still wins over the same content.
        let event = event(
            None,
            Some(
                r#"[{"type":"tool_result","isError":true,"content":"find: '/root': Permission denied"}]"#,
            ),
            None,
        );
        assert!(looks_like_tool_failure(&event));
    }

    #[test]
    fn detects_a_failure_in_a_raw_tool_message() {
        // The structured scan reads the request replay, which for a call the
        // crash drain captured holds the raw wire forms: OpenAI's
        // `role: "tool"` message and Responses' `function_call_output` item.
        // Neither matched the shape gate, so a failed tool call in an
        // interrupted conversation scored as "no deterministic tool failure".
        for payload in [
            r#"[{"role":"user","content":"run it"},
                {"role":"tool","tool_call_id":"call-1","content":"Error: command failed","is_error":true}]"#,
            r#"[{"role":"user","content":"run it"},
                {"type":"function_call_output","call_id":"call-1","output":"Error: command failed","status":"error"}]"#,
        ] {
            let event = event(Some(payload), None, None);
            assert!(
                looks_like_tool_failure(&event),
                "a failed tool result must be detected: {payload}"
            );
        }
    }

    #[test]
    fn detects_a_failure_in_a_statusless_responses_output() {
        // The Responses raw wire form carries its payload in `output`, with
        // neither a status nor an error flag — the shape the local collector
        // writes for a Codex rollout and the resource timeline replays. The
        // fallback scan never read that key, so the failed call scored as a
        // clean result.
        let event = event(
            Some(
                r#"[{"role":"user","content":"run it"},
                    {"type":"function_call_output","call_id":"call-1","output":"Error: command failed"}]"#,
            ),
            None,
            None,
        );

        assert!(looks_like_tool_failure(&event));
    }

    #[test]
    fn a_clean_responses_output_stays_clean() {
        // The same wire form with a benign payload is not a failure: reading
        // `output` must not flag successful results.
        let event = event(
            Some(
                r#"[{"role":"user","content":"list"},
                    {"type":"function_call_output","call_id":"call-1","output":"file-a
"}]"#,
            ),
            None,
            None,
        );

        assert!(!looks_like_tool_failure(&event));
    }

    /// A tool result can ride as a JSON-encoded *string*: the Responses API
    /// copies `function_call_output.output` through unchanged, and an MCP
    /// result is often serialized as `{"content":…,"isError":false}` text.
    /// The string arm scanned the encoded text without reading the flag, so a
    /// result that declares success was judged by its free-form content — a
    /// search that skips an unreadable path became a `tool_failure` finding
    /// while the detector read the same payload as clean.
    #[test]
    fn a_json_string_tool_result_that_reports_success_is_not_a_failure() {
        let clean = event(
            Some(
                r#"[{"role":"user","parts":[{"type":"tool_call_response","id":"toolu_1","response":"{\"content\":[{\"type\":\"text\",\"text\":\"grep: no such file or directory\"}],\"isError\":false}"}]}]"#,
            ),
            Some(r#"[{"role":"assistant","parts":[{"type":"text","content":"noted"}]}]"#),
            None,
        );
        assert!(
            !looks_like_tool_failure(&clean),
            "a JSON string that declares success is not a failure"
        );

        // The same encoded payload with the failure flag still reports it.
        let failed = event(
            Some(
                r#"[{"role":"user","parts":[{"type":"tool_call_response","id":"toolu_1","response":"{\"content\":[{\"type\":\"text\",\"text\":\"grep: no such file or directory\"}],\"isError\":true}"}]}]"#,
            ),
            Some(r#"[{"role":"assistant","parts":[{"type":"text","content":"noted"}]}]"#),
            None,
        );
        assert!(looks_like_tool_failure(&failed));
    }

    #[test]
    fn an_error_status_wins_over_a_declared_success() {
        // `tool_response_failure_text` treats an `error` status as overriding
        // a success flag; the grader diverged by returning the flag before
        // ever reading `status`, so the contradiction scored as success.
        for payload in [
            r#"[{"type":"tool_result","is_error":false,"status":"error","content":"done"}]"#,
            r#"[{"type":"tool_result","success":true,"status":"error","content":"done"}]"#,
        ] {
            let event = event(None, Some(payload), None);
            assert!(
                looks_like_tool_failure(&event),
                "an error status wins over the success flag: {payload}"
            );
        }

        // Controls: without the contradicting status the same flags keep
        // their plain meaning, and a plain failure is still a failure.
        let clean = event(
            None,
            Some(
                r#"[{"type":"tool_result","is_error":false,"content":"find: '/root': Permission denied"}]"#,
            ),
            None,
        );
        assert!(!looks_like_tool_failure(&clean));

        let failed = event(
            None,
            Some(r#"[{"type":"tool_result","is_error":true,"status":"ok","content":"done"}]"#),
            None,
        );
        assert!(looks_like_tool_failure(&failed));
    }

    #[test]
    fn ignores_tool_failure_text_in_user_prompt() {
        let event = event(
            Some(
                r#"[{"role":"user","content":"please quote this: tool_call_response: {\"error\":\"failed\", \"traceback\":\"FileNotFoundError\"}"}]"#,
            ),
            Some(r#"[{"role":"assistant","content":"quoted text omitted"}]"#),
            None,
        );

        assert!(!looks_like_tool_failure(&event));
    }

    /// The detector's twin scan treats any non-zero `exit code N` as a tool
    /// failure ("exit code 2", "exit code 127" keep firing after the
    /// `exit code 0` false positive was closed); this scan looked for the
    /// literal "exit code 1", so a command that failed with any other status
    /// scored as a clean result while the detector flagged the same record.
    #[test]
    fn detects_a_non_zero_exit_code_other_than_one() {
        for payload in [
            r#"[{"type":"tool_result","content":"make check\n(exit code 2)"}]"#,
            r#"[{"type":"tool_result","content":"tool runner (exit code 127)"}]"#,
        ] {
            let event = event(None, Some(payload), None);
            assert!(
                looks_like_tool_failure(&event),
                "a non-zero exit code is a tool failure: {payload}"
            );
        }

        // The success spelling stays clean, matching the detector.
        let clean = event(
            None,
            Some(r#"[{"type":"tool_result","content":"60 passing (exit code 0)"}]"#),
            None,
        );
        assert!(!looks_like_tool_failure(&clean));
    }

    /// The detector reads the `EPERM` / `EACCES` errno spellings in tool
    /// output as failure signals; this scan only knew the prose spellings, so
    /// a denied call that printed the errno scored as a clean tool result.
    #[test]
    fn detects_the_errno_spellings_the_detector_reads() {
        for payload in [
            r#"[{"type":"tool_result","content":"open /etc/shadow: EACCES"}]"#,
            r#"[{"type":"tool_result","content":"mount: EPERM"}]"#,
        ] {
            let event = event(None, Some(payload), None);
            assert!(
                looks_like_tool_failure(&event),
                "an errno spelling is a tool failure: {payload}"
            );
        }
    }

    /// A passing run reports how many checks failed, and free-form output
    /// routinely mentions an exception it handled. The detector's twin scan
    /// deliberately excludes those bare words, so the same payload must not
    /// be a deterministic failure here either: before this, a clean
    /// `test result: ok. 30 passed; 0 failed` scored `tool_use` 0.45 with a
    /// `tool_failure` finding while the detector recorded no failure.
    #[test]
    fn a_passing_run_reporting_its_failure_count_is_not_a_tool_failure() {
        for payload in [
            r#"[{"type":"tool_result","content":"test result: ok. 30 passed; 0 failed"}]"#,
            r#"[{"type":"tool_result","content":"handled the exception gracefully"}]"#,
        ] {
            let event = event(None, Some(payload), None);
            assert!(
                !looks_like_tool_failure(&event),
                "a result that only mentions failures is not one: {payload}"
            );
        }

        // The request replay carries the same result in the parts shape.
        let replayed = event(
            Some(
                r#"[{"role":"user","parts":[{"type":"tool_call_response","id":"t1","response":{"content":"scanned 3 files; 0 failed checks"}}]}]"#,
            ),
            Some(r#"[{"role":"assistant","parts":[{"type":"text","content":"done"}]}]"#),
            None,
        );
        assert!(
            !looks_like_tool_failure(&replayed),
            "a replayed pass count is not a failure either"
        );
    }

    /// The trim does not silence the real signals: a traceback and an error
    /// prefix keep firing after the generic words are gone.
    #[test]
    fn the_remaining_signals_still_fire_after_the_trim() {
        for payload in [
            r#"[{"type":"tool_result","content":"Traceback (most recent call last):"}]"#,
            r#"[{"type":"tool_result","content":"Error: cannot open the config"}]"#,
        ] {
            let event = event(None, Some(payload), None);
            assert!(
                looks_like_tool_failure(&event),
                "a real signal still fires: {payload}"
            );
        }
    }
}
