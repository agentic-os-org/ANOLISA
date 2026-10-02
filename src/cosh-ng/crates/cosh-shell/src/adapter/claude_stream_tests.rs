use std::sync::{Arc, Mutex};

use crate::types::{AgentEvent, QuestionSelectionMode};

use super::claude_stream::ClaudeStreamParser;

/// Assert that a tool-use block start produced only the argument-generation
/// status for `tool`, so the caller can keep asserting on later events alone.
#[track_caller]
fn assert_tool_arguments_status(events: &[AgentEvent], tool: &str) {
    assert!(
        matches!(
            events,
            [AgentEvent::StatusChanged { phase, message, .. }]
                if phase == "tool_arguments" && message == &format!("generating tool arguments: {tool}")
        ),
        "expected only an argument-generation status for {tool}, got {events:?}"
    );
}

#[test]
fn claude_stream_parser_extracts_partial_text_and_completion() {
    let mut parser = ClaudeStreamParser::new("run-1".to_string(), None);

    let first = parser.parse_line(
        r#"{"type":"stream_event","event":{"type":"content_block_delta","delta":{"type":"text_delta","text":"你好"}}}"#,
    );
    let second = parser.parse_line(
        r#"{"type":"stream_event","event":{"type":"content_block_delta","delta":{"type":"text_delta","text":"，shell"}}}"#,
    );
    let done = parser.parse_line(r#"{"type":"result","subtype":"success","result":"你好，shell"}"#);

    assert!(matches!(
        &first[..],
        [AgentEvent::TextDelta { text, .. }] if text == "你好"
    ));
    assert!(matches!(
        &second[..],
        [AgentEvent::TextDelta { text, .. }] if text == "，shell"
    ));
    assert!(done
        .iter()
        .any(|event| matches!(event, AgentEvent::AgentCompleted { .. })));
}

#[test]
fn claude_stream_parser_maps_thinking_delta_to_transient_status() {
    let mut parser = ClaudeStreamParser::new("run-1".to_string(), None);

    let events = parser.parse_line(
        r#"{"type":"stream_event","event":{"type":"content_block_delta","delta":{"type":"thinking_delta","thinking":"hidden reasoning chunk"}}}"#,
    );

    assert!(matches!(
        &events[..],
        [AgentEvent::StatusChanged { phase, message, .. }]
            if phase == "thinking" && message == "thinking"
    ));
    assert!(!matches!(
        &events[..],
        [AgentEvent::TextDelta { text, .. }] if text.contains("hidden reasoning chunk")
    ));
}

#[test]
fn claude_stream_parser_extracts_cumulative_assistant_snapshots() {
    let mut parser = ClaudeStreamParser::new("run-1".to_string(), None);

    let first = parser.parse_line(
        r#"{"type":"assistant","message":{"content":[{"type":"text","text":"hello"}]}}"#,
    );
    let second = parser.parse_line(
        r#"{"type":"assistant","message":{"content":[{"type":"text","text":"hello world"}]}}"#,
    );

    assert!(matches!(
        &first[..],
        [AgentEvent::TextDelta { text, .. }] if text == "hello"
    ));
    assert!(matches!(
        &second[..],
        [AgentEvent::TextDelta { text, .. }] if text == " world"
    ));
}

#[test]
fn claude_stream_parser_ignores_non_json_stdout_noise() {
    let mut parser = ClaudeStreamParser::new("run-1".to_string(), None);

    let events = parser.parse_line("Press ENTER or type command to continue");

    assert!(events.is_empty());
}

#[test]
fn claude_stream_parser_extracts_system_status() {
    let mut parser = ClaudeStreamParser::new("run-1".to_string(), None);

    let events = parser.parse_line(r#"{"type":"system","subtype":"status","status":"requesting"}"#);

    assert!(matches!(
        &events[..],
        [AgentEvent::StatusChanged { phase, message, .. }]
            if phase == "requesting" && message == "model status: requesting"
    ));
}

#[test]
fn claude_stream_parser_does_not_commit_non_resumable_session_ids() {
    let pending = Arc::new(Mutex::new(None));
    let mut parser = ClaudeStreamParser::new("run-1".to_string(), Some(Arc::clone(&pending)));

    parser.parse_line(
        r#"{"type":"system","subtype":"init","session_id":"00000000-0000-4000-8000-000000000000","session_resumable":false,"model":"mock","tools":[]}"#,
    );
    parser.parse_line(
        r#"{"type":"result","subtype":"success","session_id":"00000000-0000-4000-8000-000000000000","is_error":false,"result":"done"}"#,
    );

    assert_eq!(parser.session_resumable(), Some(false));
    assert_eq!(pending.lock().expect("pending session").as_deref(), None);
}

#[test]
fn claude_stream_parser_extracts_startup_progress_without_hook_noise() {
    let mut parser = ClaudeStreamParser::new("run-1".to_string(), None);

    let first = parser.parse_line(
        r#"{"type":"system","subtype":"hook_started","hook_name":"SessionStart:startup"}"#,
    );
    let second = parser.parse_line(
        r#"{"type":"system","subtype":"hook_started","hook_name":"SessionStart:startup"}"#,
    );
    let init = parser.parse_line(r#"{"type":"system","subtype":"init","model":"claude-opus-4-6"}"#);

    assert!(matches!(
        &first[..],
        [AgentEvent::StatusChanged { phase, message, .. }]
            if phase == "initializing" && message.contains("preparing")
    ));
    assert!(second.is_empty());
    assert!(matches!(
        &init[..],
        [AgentEvent::StatusChanged { phase, message, .. }]
            if phase == "initialized" && message.contains("claude-opus-4-6")
    ));
}

#[test]
fn claude_stream_parser_extracts_result_errors_array() {
    let mut parser = ClaudeStreamParser::new("run-1".to_string(), None);

    let events = parser.parse_line(
        r#"{"type":"result","subtype":"error_max_budget_usd","is_error":true,"errors":["Reached maximum budget ($0.05)"]}"#,
    );

    assert!(matches!(
        &events[..],
        [AgentEvent::AgentFailed { error, .. }] if error.contains("Reached maximum budget")
    ));
}

#[test]
fn claude_stream_parser_carries_structured_session_error_code() {
    let mut parser = ClaudeStreamParser::new("run-session-error".to_string(), None);

    let events = parser.parse_line(
        r#"{"type":"result","subtype":"error","is_error":true,"errors":["load failed"],"session_error_code":"not_found","session_error_phase":"load"}"#,
    );

    assert!(matches!(
        events.as_slice(),
        [AgentEvent::AgentFailed { error, .. }] if error == "load failed"
    ));
    assert_eq!(parser.session_error_code(), Some("not_found"));
    assert_eq!(parser.session_error_phase(), Some("load"));
}

#[test]
fn claude_stream_parser_carries_structured_result_error_code() {
    let mut parser = ClaudeStreamParser::new("run-result-error".to_string(), None);

    let events = parser.parse_line(
        r#"{"type":"result","subtype":"error","is_error":true,"errors":["turn limit"],"error_code":"max_turns","max_turns":50}"#,
    );

    assert!(matches!(
        events.as_slice(),
        [AgentEvent::AgentFailed {
            error,
            error_code,
            max_turns,
            ..
        }] if error == "turn limit"
            && error_code.as_deref() == Some("max_turns")
            && *max_turns == Some(50)
    ));
    assert_eq!(parser.max_turns(), Some(50));
}

#[test]
fn claude_stream_parser_extracts_tool_use_and_session_id() {
    let session = Arc::new(Mutex::new(None));
    let mut parser = ClaudeStreamParser::new("run-1".to_string(), Some(Arc::clone(&session)));

    let events = parser.parse_line(
        r#"{"type":"assistant","session_id":"session-123","message":{"content":[{"type":"tool_use","id":"tool-1","name":"Bash","input":{"command":"pwd","description":"Print working directory"}}]}}"#,
    );

    assert!(matches!(
        &events[..],
        [AgentEvent::ToolCall { name, input, .. }] if name == "Bash" && input == "pwd"
    ));
    assert_eq!(
        session.lock().expect("session lock").as_deref(),
        Some("session-123")
    );
}

#[test]
fn claude_stream_parser_extracts_streaming_bash_tool_use() {
    let mut parser = ClaudeStreamParser::new("run-1".to_string(), None);

    assert_tool_arguments_status(
        &parser.parse_line(
            r#"{"type":"stream_event","event":{"type":"content_block_start","index":1,"content_block":{"type":"tool_use","id":"toolu_stream","name":"Bash","input":{}}}}"#
        ),
        "Bash",
    );
    assert!(parser
        .parse_line(
            r#"{"type":"stream_event","event":{"type":"content_block_delta","index":1,"delta":{"type":"input_json_delta","partial_json":"{\"command\":\"pwd\"}"}}}"#
        )
        .is_empty());
    let events = parser
        .parse_line(r#"{"type":"stream_event","event":{"type":"content_block_stop","index":1}}"#);

    assert!(matches!(
        &events[..],
        [AgentEvent::ToolCall { name, input, .. }] if name == "Bash" && input == "pwd"
    ));
}

#[test]
fn streamed_tool_arguments_report_progress_without_leaking_the_payload() {
    let mut parser = ClaudeStreamParser::new("run-1".to_string(), None);

    let start = parser.parse_line(
        r#"{"type":"stream_event","event":{"type":"content_block_start","index":0,"content_block":{"type":"tool_use","id":"toolu_big","name":"write_file","input":{}}}}"#,
    );

    let [AgentEvent::StatusChanged { phase, message, .. }] = &start[..] else {
        panic!("expected one status event, got {start:?}");
    };
    assert_eq!(phase, "tool_arguments");
    assert_eq!(message, "generating tool arguments: write_file");

    // A thousand deltas would be a refresh storm, so none of them may produce an
    // event; and no status may carry the path or the file body they contain.
    for chunk in [
        r#"{"path":"/etc/secret.conf","#,
        r#""content":"TOKEN=hunter2"#,
        r#"UNTERMINATED"#,
    ] {
        let line = serde_json::json!({
            "type": "stream_event",
            "event": {
                "type": "content_block_delta",
                "index": 0,
                "delta": {"type": "input_json_delta", "partial_json": chunk}
            }
        })
        .to_string();
        assert!(
            parser.parse_line(&line).is_empty(),
            "argument delta {chunk:?} must not produce a UI event"
        );
    }

    let rendered = format!("{start:?}");
    assert!(!rendered.contains("/etc/secret.conf"), "{rendered}");
    assert!(!rendered.contains("hunter2"), "{rendered}");
}

#[test]
fn streamed_tool_argument_status_neutralizes_a_hostile_tool_name() {
    let mut parser = ClaudeStreamParser::new("run-1".to_string(), None);

    // The name is model output. A `\u001b` on the wire deserializes to a real ESC
    // byte, and the status animation writes its label straight to the terminal.
    let line = serde_json::json!({
        "type": "stream_event",
        "event": {
            "type": "content_block_start",
            "index": 0,
            "content_block": {
                "type": "tool_use",
                "id": "toolu_evil",
                "name": "write\u{1b}[2J\r\nfile"
            }
        }
    })
    .to_string();
    let events = parser.parse_line(&line);

    let [AgentEvent::StatusChanged { message, .. }] = &events[..] else {
        panic!("expected one status event, got {events:?}");
    };
    assert!(!message.contains('\u{1b}'), "{message:?}");
    assert!(!message.contains('\r'), "{message:?}");
    assert!(!message.contains('\n'), "{message:?}");
    // The escape body survives as inert text; only the ESC that armed it is gone.
    assert_eq!(message, "generating tool arguments: write[2Jfile");

    let mut parser = ClaudeStreamParser::new("run-1".to_string(), None);
    let line = serde_json::json!({
        "type": "stream_event",
        "event": {
            "type": "content_block_start",
            "index": 0,
            "content_block": {"type": "tool_use", "id": "toolu_long", "name": "n".repeat(4096)}
        }
    })
    .to_string();
    let events = parser.parse_line(&line);

    let [AgentEvent::StatusChanged { message, .. }] = &events[..] else {
        panic!("expected one status event, got {events:?}");
    };
    assert!(
        message.chars().count() < 120,
        "one status line must not flood the surface: {}",
        message.chars().count()
    );
}

#[test]
fn claude_stream_parser_keeps_text_delta_streaming_before_streamed_tool_use() {
    let mut parser = ClaudeStreamParser::new("run-1".to_string(), None);

    assert!(parser
        .parse_line(r#"{"type":"stream_event","event":{"type":"message_start"}}"#)
        .is_empty());
    let text_events = parser
        .parse_line(
            r#"{"type":"stream_event","event":{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"PRE TOOL TEXT"}}}"#
        );
    assert!(matches!(
        &text_events[..],
        [AgentEvent::TextDelta { text, .. }] if text == "PRE TOOL TEXT"
    ));
    assert_tool_arguments_status(
        &parser.parse_line(
            r#"{"type":"stream_event","event":{"type":"content_block_start","index":1,"content_block":{"type":"tool_use","id":"toolu_stream","name":"Bash","input":{}}}}"#
        ),
        "Bash",
    );
    assert!(parser
        .parse_line(
            r#"{"type":"stream_event","event":{"type":"content_block_delta","index":1,"delta":{"type":"input_json_delta","partial_json":"{\"command\":\"pwd\"}"}}}"#
        )
        .is_empty());

    let tool_events = parser
        .parse_line(r#"{"type":"stream_event","event":{"type":"content_block_stop","index":1}}"#);

    assert!(matches!(
        &tool_events[..],
        [AgentEvent::ToolCall { name, input, .. }] if name == "Bash" && input == "pwd"
    ));
    assert!(parser
        .parse_line(r#"{"type":"stream_event","event":{"type":"message_stop"}}"#)
        .is_empty());
}

#[test]
fn claude_stream_parser_deduplicates_streamed_final_snapshot_after_tool() {
    let mut parser = ClaudeStreamParser::new("run-1".to_string(), None);

    assert!(parser
        .parse_line(r#"{"type":"stream_event","event":{"type":"message_start"}}"#)
        .is_empty());
    assert!(matches!(
        &parser.parse_line(
            r#"{"type":"stream_event","event":{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"PRE"}}}"#
        )[..],
        [AgentEvent::TextDelta { text, .. }] if text == "PRE"
    ));
    assert_tool_arguments_status(
        &parser.parse_line(
            r#"{"type":"stream_event","event":{"type":"content_block_start","index":1,"content_block":{"type":"tool_use","id":"toolu_stream","name":"Bash","input":{}}}}"#
        ),
        "Bash",
    );
    assert!(parser
        .parse_line(
            r#"{"type":"stream_event","event":{"type":"content_block_delta","index":1,"delta":{"type":"input_json_delta","partial_json":"{\"command\":\"pwd\"}"}}}"#
        )
        .is_empty());
    assert!(matches!(
        &parser.parse_line(r#"{"type":"stream_event","event":{"type":"content_block_stop","index":1}}"#)[..],
        [AgentEvent::ToolCall { name, input, .. }] if name == "Bash" && input == "pwd"
    ));
    assert!(parser
        .parse_line(r#"{"type":"stream_event","event":{"type":"message_stop"}}"#)
        .is_empty());

    assert!(parser
        .parse_line(r#"{"type":"stream_event","event":{"type":"message_start"}}"#)
        .is_empty());
    let streamed_final = parser.parse_line(
        r#"{"type":"stream_event","event":{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"FINAL_SENTINEL"}}}"#,
    );
    let snapshot_final = parser.parse_line(
        r#"{"type":"assistant","message":{"content":[{"type":"text","text":"FINAL_SENTINEL"}]}}"#,
    );

    assert!(matches!(
        &streamed_final[..],
        [AgentEvent::TextDelta { text, .. }] if text == "FINAL_SENTINEL"
    ));
    assert!(snapshot_final.is_empty());
}

#[test]
fn claude_stream_parser_maps_tool_result_to_output_and_completion() {
    let mut parser = ClaudeStreamParser::new("run-1".to_string(), None);

    let events = parser.parse_line(
        r#"{"type":"user","message":{"content":[{"tool_use_id":"toolu_1","type":"tool_result","content":"/Users/quejianming/vscode/cosh-ng","is_error":false}]}}"#,
    );

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
        ] if tool_id == "toolu_1"
            && completed_id == "toolu_1"
            && stream == "stdout"
            && text == "/Users/quejianming/vscode/cosh-ng"
            && status == "success"
    ));
}

#[test]
fn claude_stream_parser_maps_error_tool_result_to_stderr() {
    let mut parser = ClaudeStreamParser::new("run-1".to_string(), None);

    let events = parser.parse_line(
        r#"{"type":"user","message":{"content":[{"type":"tool_result","content":"Denied by preflight test","is_error":true,"tool_use_id":"toolu_deny"}]}}"#,
    );

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
        ] if tool_id == "toolu_deny"
            && completed_id == "toolu_deny"
            && stream == "stderr"
            && text == "Denied by preflight test"
            && status == "error"
    ));
}

#[test]
fn claude_stream_parser_maps_tool_use_result_stdout_and_stderr_fields() {
    let mut parser = ClaudeStreamParser::new("run-1".to_string(), None);

    let events = parser.parse_line(
        r#"{"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"toolu_io","stdout":"ok\n","stderr":"warn\n","is_error":false}]}}"#,
    );

    assert!(matches!(
        &events[..],
        [
            AgentEvent::ToolOutputDelta {
                tool_id: stdout_id,
                stream: stdout_stream,
                text: stdout,
                ..
            },
            AgentEvent::ToolOutputDelta {
                tool_id: stderr_id,
                stream: stderr_stream,
                text: stderr,
                ..
            },
            AgentEvent::ToolCompleted { tool_id, status, .. }
        ] if stdout_id == "toolu_io"
            && stderr_id == "toolu_io"
            && tool_id == "toolu_io"
            && stdout_stream == "stdout"
            && stderr_stream == "stderr"
            && stdout == "ok\n"
            && stderr == "warn\n"
            && status == "error"
    ));
}

#[test]
fn claude_stream_parser_maps_interrupted_tool_result_status() {
    let mut parser = ClaudeStreamParser::new("run-1".to_string(), None);

    let events = parser.parse_line(
        r#"{"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"toolu_int","content":"interrupted by user","status":"interrupted"}]}}"#,
    );

    assert!(matches!(
        &events[..],
        [
            AgentEvent::ToolOutputDelta { stream, text, .. },
            AgentEvent::ToolCompleted { tool_id, status, .. }
        ] if stream == "stderr"
            && text == "interrupted by user"
            && tool_id == "toolu_int"
            && status == "interrupted"
    ));
}

#[test]
fn claude_stream_parser_bounds_large_tool_result() {
    let mut parser = ClaudeStreamParser::new("run-1".to_string(), None);
    let large = "x".repeat(4_010);
    let line = serde_json::json!({
        "type": "user",
        "message": {
            "content": [{
                "type": "tool_result",
                "tool_use_id": "toolu_large",
                "content": large,
                "is_error": false
            }]
        }
    })
    .to_string();

    let events = parser.parse_line(&line);

    assert!(matches!(
        &events[..],
        [
            AgentEvent::ToolOutputDelta { text, .. },
            AgentEvent::ToolCompleted { .. }
        ] if text.len() < 4_100 && text.contains("10 chars omitted")
    ));
}

#[test]
fn claude_stream_parser_keeps_native_and_control_question_paths_distinct() {
    let mut parser = ClaudeStreamParser::new("run-1".to_string(), None);

    let events = parser.parse_line(
        r#"{"type":"assistant","message":{"content":[{"type":"tool_use","id":"tool-ask","name":"AskUserQuestion","input":{"question":"Pick a color","options":[{"label":"Green"},{"label":"Blue"}],"allow_free_text":false}}]}}"#,
    );

    assert!(matches!(
        &events[..],
        [AgentEvent::UserQuestion {
            question,
            options,
            allow_free_text,
            selection_mode,
            ..
        }] if question == "Pick a color"
            && options == &vec!["Green".to_string(), "Blue".to_string()]
            && !allow_free_text
            && *selection_mode == QuestionSelectionMode::Single
    ));

    let events = parser.parse_line(
        r#"{"type":"assistant","message":{"content":[{"type":"tool_use","id":"tool-core-ask","name":"ask_user_question","input":{"question":"Handled by control"}}]}}"#,
    );
    assert!(matches!(
        &events[..],
        [AgentEvent::ToolCall { name, .. }] if name == "ask_user_question"
    ));
}

#[test]
fn claude_stream_parser_maps_nested_ask_user_question_options() {
    let mut parser = ClaudeStreamParser::new("run-1".to_string(), None);

    let events = parser.parse_line(
        r#"{"type":"assistant","message":{"content":[{"type":"tool_use","id":"tool-ask","name":"AskUserQuestion","input":{"questions":[{"question":"你喜欢什么颜色？","header":"颜色偏好","options":[{"label":"白色","description":"White"},{"label":"黑色","description":"Black"},{"label":"蓝色","description":"Blue"},{"label":"红色","description":"Red"}],"multiSelect":false}]}}]}}"#,
    );

    assert!(matches!(
        &events[..],
        [AgentEvent::UserQuestion {
            question,
            options,
            allow_free_text,
            selection_mode,
            ..
        }] if question == "你喜欢什么颜色？"
            && options == &vec![
                "白色".to_string(),
                "黑色".to_string(),
                "蓝色".to_string(),
                "红色".to_string()
            ]
            && *allow_free_text
            && *selection_mode == QuestionSelectionMode::Single
    ));
}

#[test]
fn claude_stream_parser_waits_for_streamed_question_input_json() {
    let mut parser = ClaudeStreamParser::new("run-1".to_string(), None);

    let start_events = parser.parse_line(
        r#"{"type":"stream_event","event":{"type":"content_block_start","index":0,"content_block":{"type":"tool_use","id":"tool-ask","name":"AskUserQuestion","input":{}}}}"#,
    );
    assert_tool_arguments_status(&start_events, "AskUserQuestion");

    let delta_events = parser.parse_line(
        r#"{"type":"stream_event","event":{"type":"content_block_delta","index":0,"delta":{"type":"input_json_delta","partial_json":"{\"questions\":[{\"question\":\"你喜欢什么颜色？\",\"header\":\"颜色\",\"options\":[{\"label\":\"白色\"},{\"label\":\"黑色\"}],\"multiSelect\":false}]}"}}}"#,
    );
    assert!(delta_events.is_empty());

    let stop_events = parser
        .parse_line(r#"{"type":"stream_event","event":{"type":"content_block_stop","index":0}}"#);

    assert!(matches!(
        &stop_events[..],
        [AgentEvent::UserQuestion {
            question,
            options,
            allow_free_text,
            selection_mode,
            ..
        }] if question == "你喜欢什么颜色？"
            && options == &vec!["白色".to_string(), "黑色".to_string()]
            && *allow_free_text
            && *selection_mode == QuestionSelectionMode::Single
    ));
}

#[test]
fn claude_stream_parser_accumulates_fragmented_question_input_json() {
    let mut parser = ClaudeStreamParser::new("run-1".to_string(), None);

    assert_tool_arguments_status(
        &parser.parse_line(
            r#"{"type":"stream_event","event":{"type":"content_block_start","index":1,"content_block":{"type":"tool_use","id":"tool-ask","name":"AskUserQuestion","input":{}}}}"#
        ),
        "AskUserQuestion",
    );

    for partial_json in [
        r#"{"questions":[{"#,
        r#""question":"你喜欢什么颜色？","#,
        r#""header":"颜色","#,
        r#""options":[{"label":"白色","description":"白色"},"#,
        r#"{"label":"黑色","description":"黑色"},"#,
        r#"{"label":"蓝色","description":"蓝色"},"#,
        r#"{"label":"红色","description":"红色"}],"#,
        r#""multiSelect":false}]}"#,
    ] {
        let line = serde_json::json!({
            "type": "stream_event",
            "event": {
                "type": "content_block_delta",
                "index": 1,
                "delta": {
                    "type": "input_json_delta",
                    "partial_json": partial_json
                }
            }
        })
        .to_string();
        assert!(parser.parse_line(&line).is_empty());
    }

    let stop_events = parser
        .parse_line(r#"{"type":"stream_event","event":{"type":"content_block_stop","index":1}}"#);

    assert!(matches!(
        &stop_events[..],
        [AgentEvent::UserQuestion {
            question,
            options,
            allow_free_text,
            selection_mode,
            ..
        }] if question == "你喜欢什么颜色？"
            && options == &vec![
                "白色".to_string(),
                "黑色".to_string(),
                "蓝色".to_string(),
                "红色".to_string()
            ]
            && *allow_free_text
            && *selection_mode == QuestionSelectionMode::Single
    ));
}

#[test]
fn claude_stream_parser_deduplicates_streamed_and_snapshot_tool_use() {
    let mut parser = ClaudeStreamParser::new("run-1".to_string(), None);

    assert_tool_arguments_status(
        &parser.parse_line(
            r#"{"type":"stream_event","event":{"type":"content_block_start","index":0,"content_block":{"type":"tool_use","id":"tool-ask","name":"AskUserQuestion","input":{}}}}"#
        ),
        "AskUserQuestion",
    );
    assert!(parser
        .parse_line(
            r#"{"type":"stream_event","event":{"type":"content_block_delta","index":0,"delta":{"type":"input_json_delta","partial_json":"{\"questions\":[{\"question\":\"Pick\",\"header\":\"Pick\",\"options\":[{\"label\":\"A\"},{\"label\":\"B\"}],\"multiSelect\":false}]}"}}}"#,
        )
        .is_empty());

    let snapshot_events = parser.parse_line(
        r#"{"type":"assistant","message":{"content":[{"type":"tool_use","id":"tool-ask","name":"AskUserQuestion","input":{"questions":[{"question":"Pick","header":"Pick","options":[{"label":"A"},{"label":"B"}],"multiSelect":false}]}}]}}"#,
    );
    let stop_events = parser
        .parse_line(r#"{"type":"stream_event","event":{"type":"content_block_stop","index":0}}"#);

    assert!(snapshot_events.is_empty());
    assert!(matches!(
        &stop_events[..],
        [AgentEvent::UserQuestion {
            question,
            options,
            allow_free_text,
            selection_mode,
            ..
        }] if question == "Pick"
            && options == &vec!["A".to_string(), "B".to_string()]
            && *allow_free_text
            && *selection_mode == QuestionSelectionMode::Single
    ));
}

#[test]
fn claude_stream_parser_ignores_incomplete_snapshot_for_streaming_question() {
    let mut parser = ClaudeStreamParser::new("run-1".to_string(), None);

    assert_tool_arguments_status(
        &parser.parse_line(
            r#"{"type":"stream_event","event":{"type":"content_block_start","index":0,"content_block":{"type":"tool_use","id":"tool-ask","name":"AskUserQuestion","input":{}}}}"#
        ),
        "AskUserQuestion",
    );

    let snapshot_events = parser.parse_line(
        r#"{"type":"assistant","message":{"content":[{"type":"text","text":"Agent needs your input"},{"type":"tool_use","id":"tool-ask","name":"AskUserQuestion","input":{}}]}}"#,
    );
    assert!(snapshot_events.is_empty());

    assert!(parser
        .parse_line(
            r#"{"type":"stream_event","event":{"type":"content_block_delta","index":0,"delta":{"type":"input_json_delta","partial_json":"{\"questions\":[{\"question\":\"你喜欢什么颜色？\",\"header\":\"颜色\",\"options\":[{\"label\":\"白色\"},{\"label\":\"黑色\"},{\"label\":\"蓝色\"}],\"multiSelect\":false}]}"}}}"#,
        )
        .is_empty());

    let stop_events = parser
        .parse_line(r#"{"type":"stream_event","event":{"type":"content_block_stop","index":0}}"#);

    assert!(matches!(
        &stop_events[..],
        [AgentEvent::UserQuestion {
            question,
            options,
            allow_free_text,
            selection_mode,
            ..
        }] if question == "你喜欢什么颜色？"
            && options == &vec![
                "白色".to_string(),
                "黑色".to_string(),
                "蓝色".to_string()
            ]
            && *allow_free_text
            && *selection_mode == QuestionSelectionMode::Single
    ));
}

#[test]
fn claude_stream_parser_waits_for_permission_request_after_empty_question_snapshot() {
    let mut parser = ClaudeStreamParser::new("run-1".to_string(), None);

    let snapshot_events = parser.parse_line(
        r#"{"type":"assistant","message":{"content":[{"type":"tool_use","id":"tool-ask","name":"AskUserQuestion","input":{}}]}}"#,
    );
    assert!(snapshot_events.is_empty());

    let permission_events = parser.parse_line(
        r#"{"event":"permission_request","toolName":"AskUserQuestion","input":{"questions":[{"question":"你喜欢什么颜色？","header":"颜色","options":[{"label":"白色","description":"White"},{"label":"黑色","description":"Black"},{"label":"蓝色","description":"Blue"}],"multiSelect":false}]},"permissionLevel":null}"#,
    );

    assert!(matches!(
        &permission_events[..],
        [AgentEvent::UserQuestion {
            question,
            options,
            allow_free_text,
            selection_mode,
            ..
        }] if question == "你喜欢什么颜色？"
            && options == &vec![
                "白色".to_string(),
                "黑色".to_string(),
                "蓝色".to_string()
            ]
            && *allow_free_text
            && *selection_mode == QuestionSelectionMode::Single
    ));
}

#[test]
fn claude_stream_parser_deduplicates_tool_input_and_permission_request_question() {
    let mut parser = ClaudeStreamParser::new("run-1".to_string(), None);
    let tool_input = r#"{"event":"tool_input","toolName":"AskUserQuestion","input":{"questions":[{"question":"选择检查项","header":"检查","options":[{"label":"Lint"},{"label":"Unit tests"}],"multiSelect":true}]},"toolUseId":"toolu_ask"}"#;
    let permission_request = r#"{"event":"permission_request","toolName":"AskUserQuestion","input":{"questions":[{"question":"选择检查项","header":"检查","options":[{"label":"Lint"},{"label":"Unit tests"}],"multiSelect":true}]},"permissionLevel":null}"#;

    let tool_events = parser.parse_line(tool_input);
    let permission_events = parser.parse_line(permission_request);

    assert!(matches!(
        &tool_events[..],
        [AgentEvent::UserQuestion {
            question,
            options,
            selection_mode,
            ..
        }] if question == "选择检查项"
            && options == &vec!["Lint".to_string(), "Unit tests".to_string()]
            && *selection_mode == QuestionSelectionMode::Multiple
    ));
    assert!(permission_events.is_empty());
}

#[test]
fn claude_stream_parser_maps_question_options_with_title_and_value() {
    let mut parser = ClaudeStreamParser::new("run-1".to_string(), None);

    let events = parser.parse_line(
        r#"{"type":"assistant","message":{"content":[{"type":"tool_use","id":"tool-ask","name":"AskUserQuestion","input":{"question":"Pick one","suggestions":[{"title":"Fast path"},{"value":"Manual path"}],"allowFreeText":false}}]}}"#,
    );

    assert!(matches!(
        &events[..],
        [AgentEvent::UserQuestion {
            question,
            options,
            allow_free_text,
            ..
        }] if question == "Pick one"
            && options == &vec!["Fast path".to_string(), "Manual path".to_string()]
            && !allow_free_text
    ));
}

#[test]
fn claude_stream_parser_maps_question_text_context_when_tool_input_is_empty() {
    let mut parser = ClaudeStreamParser::new("run-1".to_string(), None);

    let events = parser.parse_line(
        r#"{"type":"assistant","message":{"content":[{"type":"text","text":"你喜欢什么动物？\n猫 狗 兔 鸟"},{"type":"tool_use","id":"tool-ask","name":"AskUserQuestion","input":{}}]}}"#,
    );

    assert!(matches!(
        &events[..],
        [AgentEvent::UserQuestion {
            question,
            options,
            allow_free_text,
            selection_mode,
            ..
        }] if question == "你喜欢什么动物？"
            && options == &vec![
                "猫".to_string(),
                "狗".to_string(),
                "兔".to_string(),
                "鸟".to_string()
            ]
            && *allow_free_text
            && *selection_mode == QuestionSelectionMode::Single
    ));
}

#[test]
fn claude_stream_parser_maps_multi_select_question_mode() {
    let mut parser = ClaudeStreamParser::new("run-1".to_string(), None);

    let events = parser.parse_line(
        r#"{"type":"assistant","message":{"content":[{"type":"tool_use","id":"tool-ask","name":"AskUserQuestion","input":{"question":"Pick checks","options":["Lint","Test"],"multi_select":true,"allow_free_text":true}}]}}"#,
    );

    assert!(matches!(
        &events[..],
        [AgentEvent::UserQuestion {
            question,
            options,
            allow_free_text,
            selection_mode,
            ..
        }] if question == "Pick checks"
            && options == &vec!["Lint".to_string(), "Test".to_string()]
            && *allow_free_text
            && *selection_mode == QuestionSelectionMode::Multiple
    ));
}
