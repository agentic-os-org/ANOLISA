use std::sync::{Arc, Mutex};

use serde_json::Value;

use crate::types::{AgentEvent, QuestionSelectionMode};

use super::claude_stream::ClaudeStreamParser;
use super::AdapterError;

pub(super) struct QwenStreamParser {
    run_id: String,
    inner: ClaudeStreamParser,
    pending_text_prefix: String,
    pending_synthetic_question: Option<String>,
}

impl QwenStreamParser {
    pub(super) fn new(run_id: String, session_state: Option<Arc<Mutex<Option<String>>>>) -> Self {
        Self {
            run_id: run_id.clone(),
            inner: ClaudeStreamParser::new(run_id, session_state),
            pending_text_prefix: String::new(),
            pending_synthetic_question: None,
        }
    }

    pub(super) fn parse_line(&mut self, line: &str) -> Vec<AgentEvent> {
        let events = self.inner.parse_line(line);
        let mut localized = Vec::new();
        for event in events {
            localized.extend(self.localize_event(event));
        }
        localized
    }

    pub(super) fn finish(
        &mut self,
        sink: &mut dyn FnMut(AgentEvent) -> Result<(), AdapterError>,
    ) -> Result<(), AdapterError> {
        let mut collected = Vec::new();
        self.inner.finish(&mut |event| {
            collected.push(event);
            Ok(())
        })?;
        for event in collected {
            for event in self.localize_event(event) {
                sink(event)?;
            }
        }
        if let Some(text) = self.pending_synthetic_question.take() {
            sink(AgentEvent::TextDelta {
                run_id: self.run_id.clone(),
                text: format!("{text}\n\n[debug: failed to parse COSH_QUESTION JSON]"),
            })?;
        }
        Ok(())
    }

    fn localize_event(&mut self, event: AgentEvent) -> Vec<AgentEvent> {
        match event {
            AgentEvent::StatusChanged {
                run_id,
                phase,
                message,
            } => vec![AgentEvent::StatusChanged {
                run_id,
                phase,
                message: self.localize_status_message(&message),
            }],
            AgentEvent::AgentCompleted { run_id, .. } => vec![AgentEvent::AgentCompleted {
                run_id,
                summary: "analysis completed".to_string(),
            }],
            AgentEvent::AgentFailed {
                run_id,
                error,
                error_code,
                max_turns,
            } => vec![AgentEvent::AgentFailed {
                run_id,
                error: error
                    .replace("Claude Code", "model")
                    .replace("Claude", "model")
                    .replace("claude-code", "model")
                    .replace("claude code", "model"),
                error_code,
                max_turns,
            }],
            AgentEvent::TextDelta { run_id, text } => self.localize_text_delta(run_id, text),
            other => vec![other],
        }
    }

    fn localize_text_delta(&mut self, run_id: String, text: String) -> Vec<AgentEvent> {
        let marker = "COSH_QUESTION:";
        let Some(pending) = self.pending_synthetic_question.as_mut() else {
            let text = if self.pending_text_prefix.is_empty() {
                text
            } else {
                let mut combined = std::mem::take(&mut self.pending_text_prefix);
                combined.push_str(&text);
                combined
            };
            let Some((prefix, suffix)) = text.split_once(marker) else {
                let (emit, held) = split_possible_marker_prefix(&text, marker);
                self.pending_text_prefix = held.to_string();
                return if emit.is_empty() {
                    Vec::new()
                } else {
                    vec![AgentEvent::TextDelta {
                        run_id,
                        text: emit.to_string(),
                    }]
                };
            };
            let mut events = Vec::new();
            if !prefix.is_empty() {
                events.push(AgentEvent::TextDelta {
                    run_id: run_id.clone(),
                    text: prefix.to_string(),
                });
            }
            self.pending_synthetic_question = Some(format!("{marker}{suffix}"));
            return self.parse_pending_synthetic_question(run_id, events);
        };

        pending.push_str(&text);
        self.parse_pending_synthetic_question(run_id, Vec::new())
    }

    fn parse_pending_synthetic_question(
        &mut self,
        run_id: String,
        mut events: Vec<AgentEvent>,
    ) -> Vec<AgentEvent> {
        let Some(buffer) = self.pending_synthetic_question.as_ref() else {
            return events;
        };
        match complete_json_after_marker(buffer) {
            SyntheticQuestionJson::Incomplete => events,
            SyntheticQuestionJson::Complete(_) => {
                if let Some(event) = synthetic_question_from_text(&run_id, buffer) {
                    events.push(event);
                    self.pending_synthetic_question = None;
                }
                events
            }
        }
    }

    fn localize_status_message(&self, message: &str) -> String {
        message
            .replace("Claude Code", "model")
            .replace("Claude", "model")
            .replace("claude-code", "model")
            .replace("claude code", "model")
    }
}

fn split_possible_marker_prefix<'a>(text: &'a str, marker: &str) -> (&'a str, &'a str) {
    let held_len = (1..marker.len())
        .rev()
        .find(|len| text.ends_with(&marker[..*len]))
        .unwrap_or(0);
    if held_len == 0 {
        return (text, "");
    }
    text.split_at(text.len() - held_len)
}

fn synthetic_question_from_text(run_id: &str, text: &str) -> Option<AgentEvent> {
    let json_text = match complete_json_after_marker(text) {
        SyntheticQuestionJson::Complete(json_text) => json_text,
        SyntheticQuestionJson::Incomplete => return None,
    };
    let value: Value = serde_json::from_str(json_text).ok()?;
    synthetic_question_from_value(run_id, &value)
}

fn synthetic_question_from_value(run_id: &str, value: &Value) -> Option<AgentEvent> {
    let question = value.get("question")?.as_str()?.to_string();
    let options = value
        .get("options")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(|item| {
                    item.as_str()
                        .map(ToString::to_string)
                        .or_else(|| option_label(item))
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let allow_free_text = bool_field(value, "allow_free_text")
        .or_else(|| bool_field(value, "allowFreeText"))
        .unwrap_or(options.is_empty());
    let selection_mode = if bool_field(value, "multi_select")
        .or_else(|| bool_field(value, "multiSelect"))
        .unwrap_or(false)
    {
        QuestionSelectionMode::Multiple
    } else {
        QuestionSelectionMode::Single
    };

    Some(AgentEvent::UserQuestion {
        run_id: run_id.to_string(),
        provider_request_id: None,
        question,
        options,
        allow_free_text,
        selection_mode,
    })
}

enum SyntheticQuestionJson<'a> {
    Complete(&'a str),
    Incomplete,
}

fn complete_json_after_marker(text: &str) -> SyntheticQuestionJson<'_> {
    let Some((_, after_marker)) = text.split_once("COSH_QUESTION:") else {
        return SyntheticQuestionJson::Incomplete;
    };
    let Some(start_rel) = after_marker.find('{') else {
        return SyntheticQuestionJson::Incomplete;
    };
    let json_start = text.len() - after_marker.len() + start_rel;
    let mut depth = 0_i32;
    let mut in_string = false;
    let mut escaped = false;

    for (offset, ch) in text[json_start..].char_indices() {
        if in_string {
            if escaped {
                escaped = false;
            } else if ch == '\\' {
                escaped = true;
            } else if ch == '"' {
                in_string = false;
            }
            continue;
        }

        match ch {
            '"' => in_string = true,
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    let end = json_start + offset + ch.len_utf8();
                    return SyntheticQuestionJson::Complete(text[json_start..end].trim());
                }
            }
            _ => {}
        }
    }

    SyntheticQuestionJson::Incomplete
}

fn option_label(value: &Value) -> Option<String> {
    value
        .get("label")
        .or_else(|| value.get("title"))
        .or_else(|| value.get("value"))
        .and_then(Value::as_str)
        .map(ToString::to_string)
}

fn bool_field(value: &Value, key: &str) -> Option<bool> {
    value.get(key).and_then(Value::as_bool)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parser() -> QwenStreamParser {
        QwenStreamParser::new("test-run".to_string(), None)
    }

    #[test]
    fn text_delta_from_stream_event() {
        let mut p = parser();
        let events = p.parse_line(r#"{"type":"stream_event","event":{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"hello"}}}"#);
        assert_eq!(events.len(), 1);
        assert!(matches!(&events[0], AgentEvent::TextDelta { text, .. } if text == "hello"));
    }

    #[test]
    fn system_init_extracts_model() {
        let mut p = parser();
        let events = p.parse_line(r#"{"type":"system","subtype":"init","session_id":"sess-1","model":"qwen3.7-max","tools":[],"copilot_version":"2.4.1"}"#);
        assert_eq!(events.len(), 1);
        assert!(
            matches!(&events[0], AgentEvent::StatusChanged { phase, message, .. }
            if phase == "initialized" && message.contains("qwen3.7-max") && !message.contains("claude"))
        );
    }

    #[test]
    fn result_success() {
        let mut p = parser();
        let events = p.parse_line(r#"{"type":"result","subtype":"success","session_id":"s","is_error":false,"result":"done","duration_ms":100}"#);
        assert!(events
            .iter()
            .any(|e| matches!(e, AgentEvent::TextDelta { text, .. } if text == "done")));
        assert!(events.iter().any(|e| matches!(e, AgentEvent::AgentCompleted { summary, .. } if summary == "analysis completed")));
    }

    #[test]
    fn result_error() {
        let mut p = parser();
        let events = p.parse_line(r#"{"type":"result","is_error":true,"errors":["rate limit"]}"#);
        assert!(events.iter().any(
            |e| matches!(e, AgentEvent::AgentFailed { error, .. } if error.contains("rate limit"))
        ));
    }

    #[test]
    fn thinking_delta() {
        let mut p = parser();
        let events = p.parse_line(r#"{"type":"stream_event","event":{"type":"content_block_delta","index":0,"delta":{"type":"thinking_delta","thinking":"hmm"}}}"#);
        assert_eq!(events.len(), 1);
        assert!(
            matches!(&events[0], AgentEvent::StatusChanged { phase, message, .. }
            if phase == "thinking" && message == "thinking")
        );
    }

    #[test]
    fn localizes_capitalized_claude_wording() {
        let mut p = parser();
        let events = p.localize_event(AgentEvent::StatusChanged {
            run_id: "r".to_string(),
            phase: "starting".to_string(),
            message: "Starting Claude Code backend".to_string(),
        });
        assert!(
            matches!(&events[..], [AgentEvent::StatusChanged { message, .. }]
            if message == "Starting model backend")
        );
    }

    #[test]
    fn session_id_remembered() {
        let state = Arc::new(Mutex::new(None::<String>));
        let mut p = QwenStreamParser::new("r".to_string(), Some(Arc::clone(&state)));
        p.parse_line(
            r#"{"type":"system","subtype":"init","session_id":"qwen-sess-42","model":"m"}"#,
        );
        assert_eq!(*state.lock().unwrap(), Some("qwen-sess-42".to_string()));
    }

    #[test]
    fn finish_emits_completed_when_no_result() {
        let mut p = parser();
        p.parse_line(r#"{"type":"stream_event","event":{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"hi"}}}"#);
        let mut events = Vec::new();
        p.finish(&mut |e| {
            events.push(e);
            Ok(())
        })
        .unwrap();
        assert!(events.iter().any(|e| matches!(e, AgentEvent::AgentCompleted { summary, .. } if summary == "analysis completed")));
    }

    #[test]
    fn tool_call_from_assistant_snapshot() {
        let mut p = parser();
        let events = p.parse_line(r#"{"type":"assistant","session_id":"s","message":{"id":"m1","type":"message","role":"assistant","model":"qwen","content":[{"type":"tool_use","id":"call_abc","name":"run_shell_command","input":{"command":"ls -la"}}]}}"#);
        assert!(events
            .iter()
            .any(|e| matches!(e, AgentEvent::ToolCall { name, input, .. }
            if name == "run_shell_command" && input == "ls -la")));
        assert!(events.iter().any(|e| matches!(
            e,
            AgentEvent::ToolCall {
                tool_id: Some(tool_id),
                ..
            } if tool_id == "call_abc"
        )));
    }

    #[test]
    fn tool_result_from_user_snapshot() {
        let mut p = parser();
        let events = p.parse_line(r#"{"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"call_abc","is_error":false,"content":"cosh-control-protocol"}]}}"#);
        assert!(matches!(
            &events[..],
            [
                AgentEvent::ToolOutputDelta {
                    tool_id,
                    stream,
                    text,
                    ..
                },
                AgentEvent::ToolCompleted {
                    tool_id: completed_id,
                    status,
                    ..
                }
            ] if tool_id == "call_abc"
                && completed_id == "call_abc"
                && stream == "stdout"
                && text == "cosh-control-protocol"
                && status == "success"
        ));
    }

    #[test]
    fn function_call_from_qwen_message_parts() {
        let mut p = parser();
        let events = p.parse_line(r#"{"type":"assistant","message":{"role":"model","parts":[{"functionCall":{"id":"call_qwen","name":"run_shell_command","args":{"command":"memory_pressure","description":"Check memory","is_background":false}}}]}}"#);
        assert!(matches!(
            &events[..],
            [AgentEvent::ToolCall {
                tool_id: Some(tool_id),
                name,
                input,
                ..
            }] if tool_id == "call_qwen" && name == "run_shell_command" && input == "memory_pressure"
        ));
    }

    #[test]
    fn function_response_from_qwen_message_parts_uses_result_display() {
        let mut p = parser();
        let events = p.parse_line(r#"{"type":"tool_result","message":{"role":"user","parts":[{"functionResponse":{"id":"call_qwen","name":"run_shell_command","response":{"output":"Command: memory_pressure\nOutput: wrapped output"}}}]},"toolCallResult":{"callId":"call_qwen","status":"success","resultDisplay":"System-wide memory free percentage: 87%"}}"#);
        assert!(matches!(
            &events[..],
            [
                AgentEvent::ToolOutputDelta {
                    tool_id,
                    stream,
                    text,
                    ..
                },
                AgentEvent::ToolCompleted {
                    tool_id: completed_id,
                    status,
                    ..
                }
            ] if tool_id == "call_qwen"
                && completed_id == "call_qwen"
                && stream == "stdout"
                && text == "System-wide memory free percentage: 87%"
                && status == "success"
        ));
    }

    #[test]
    fn error_tool_result_from_user_snapshot() {
        let mut p = parser();
        let events = p.parse_line(r#"{"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"call_err","is_error":true,"content":"permission denied"}]}}"#);
        assert!(matches!(
            &events[..],
            [
                AgentEvent::ToolOutputDelta {
                    tool_id,
                    stream,
                    text,
                    ..
                },
                AgentEvent::ToolCompleted {
                    tool_id: completed_id,
                    status,
                    ..
                }
            ] if tool_id == "call_err"
                && completed_id == "call_err"
                && stream == "stderr"
                && text == "permission denied"
                && status == "error"
        ));
    }

    #[test]
    fn question_from_assistant_snapshot() {
        let mut p = parser();
        let events = p.parse_line(r#"{"type":"assistant","session_id":"s","message":{"id":"m1","type":"message","role":"assistant","model":"qwen","content":[{"type":"tool_use","id":"call_question","name":"AskUserQuestion","input":{"questions":[{"question":"Pick a shell","header":"Shell","options":[{"label":"bash"},{"label":"zsh"}],"multiSelect":false,"allowFreeText":false}]}}]}}"#);
        assert!(
            events.iter().any(|e| matches!(
                e,
                AgentEvent::UserQuestion {
                    question,
                    options,
                    allow_free_text,
                    ..
                } if question == "Pick a shell" && options == &vec!["bash".to_string(), "zsh".to_string()] && !allow_free_text
            )),
            "expected UserQuestion, got: {events:?}"
        );
    }

    #[test]
    fn multi_select_question_from_permission_request() {
        let mut p = parser();
        let events = p.parse_line(r#"{"event":"permission_request","toolName":"AskUserQuestion","input":{"questions":[{"question":"Choose checks","header":"Checks","options":[{"label":"Lint"},{"label":"Unit tests"}],"multiSelect":true,"allowFreeText":true}]},"permissionLevel":null}"#);
        assert!(events.iter().any(|e| matches!(
            e,
            AgentEvent::UserQuestion {
                question,
                options,
                allow_free_text,
                ..
            } if question == "Choose checks" && options == &vec!["Lint".to_string(), "Unit tests".to_string()] && *allow_free_text
        )));
    }

    #[test]
    fn free_text_only_question_from_assistant_snapshot() {
        let mut p = parser();
        let events = p.parse_line(r#"{"type":"assistant","session_id":"s","message":{"id":"m1","type":"message","role":"assistant","model":"qwen","content":[{"type":"tool_use","id":"call_question","name":"AskUserQuestion","input":{"question":"Describe the project goal","allowFreeText":true}}]}}"#);
        assert!(
            events.iter().any(|e| matches!(
                e,
                AgentEvent::UserQuestion {
                    question,
                    options,
                    allow_free_text,
                    ..
                } if question == "Describe the project goal" && options.is_empty() && *allow_free_text
            )),
            "expected free-text UserQuestion, got: {events:?}"
        );
    }

    #[test]
    fn synthetic_question_from_text_fallback() {
        let mut p = parser();
        let events = p.parse_line(r#"{"type":"assistant","session_id":"s","message":{"id":"m1","type":"message","role":"assistant","model":"qwen","content":[{"type":"text","text":"COSH_QUESTION: {\"question\":\"你喜欢什么颜色？\",\"options\":[],\"allow_free_text\":true,\"multi_select\":false}"}]}}"#);
        assert!(
            events.iter().any(|e| matches!(
                e,
                AgentEvent::UserQuestion {
                    question,
                    options,
                    allow_free_text,
                    selection_mode,
                    ..
                } if question == "你喜欢什么颜色？"
                    && options.is_empty()
                    && *allow_free_text
                    && *selection_mode == crate::types::QuestionSelectionMode::Single
            )),
            "expected synthetic UserQuestion, got: {events:?}"
        );
    }

    #[test]
    fn synthetic_question_from_multiline_text_fallback() {
        let mut p = parser();
        let events = p.parse_line(r#"{"type":"assistant","session_id":"s","message":{"id":"m1","type":"message","role":"assistant","model":"qwen","content":[{"type":"text","text":"COSH_QUESTION: {\n  \"question\":\"检索什么？\",\n  \"options\":[\"文件\",\"进程\"],\n  \"allow_free_text\":true,\n  \"multi_select\":false\n}"}]}}"#);
        assert!(events.iter().any(|e| matches!(
            e,
            AgentEvent::UserQuestion {
                question,
                options,
                allow_free_text,
                ..
            } if question == "检索什么？"
                && options == &vec!["文件".to_string(), "进程".to_string()]
                && *allow_free_text
        )));
    }

    #[test]
    fn synthetic_question_from_split_text_delta() {
        let mut p = parser();
        let first = p.parse_line(r#"{"type":"stream_event","event":{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"COSH_QUESTION: {\"question\":\"检索什么？\""}}}"#);
        assert!(first.is_empty(), "marker should be buffered: {first:?}");
        let second = p.parse_line(r#"{"type":"stream_event","event":{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":",\"options\":[\"文件\"],\"allow_free_text\":true,\"multi_select\":false}"}}}"#);
        assert!(second.iter().any(|e| matches!(
            e,
            AgentEvent::UserQuestion {
                question,
                options,
                allow_free_text,
                ..
            } if question == "检索什么？"
                && options == &vec!["文件".to_string()]
                && *allow_free_text
        )));
    }

    #[test]
    fn synthetic_question_marker_can_span_text_delta_boundary() {
        let mut p = parser();
        let first = p.parse_line(r#"{"type":"stream_event","event":{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"COSH_QUES"}}}"#);
        assert!(
            first.is_empty(),
            "partial marker should be buffered: {first:?}"
        );
        let second = p.parse_line(r#"{"type":"stream_event","event":{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"TION: {\"question\":\"Project nickname?\",\"options\":[],\"allow_free_text\":true,\"multi_select\":false}"}}}"#);
        assert!(second.iter().any(|e| matches!(
            e,
            AgentEvent::UserQuestion {
                question,
                options,
                allow_free_text,
                ..
            } if question == "Project nickname?" && options.is_empty() && *allow_free_text
        )));
    }

    #[test]
    fn extra_co_fields_ignored() {
        let mut p = parser();
        let events = p.parse_line(r#"{"type":"stream_event","uuid":"xxx","parent_tool_use_id":"yyy","message_id":"zzz","event":{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"ok"}}}"#);
        assert_eq!(events.len(), 1);
        assert!(matches!(&events[0], AgentEvent::TextDelta { text, .. } if text == "ok"));
    }
}
