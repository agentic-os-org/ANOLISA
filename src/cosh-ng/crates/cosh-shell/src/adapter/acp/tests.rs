//! Pure-logic tests for the synchronous ACP client codec and projection.

use super::codec::{AcpClientCodec, Inbound, Phase, RequestKind, MAX_FRAME_BYTES};
use super::projection::project_update;
use super::{permission_outcome, reject_by_default, AcpPermissionDecision};
use agent_client_protocol::v1::schema::{
    ContentBlock, ContentChunk, PermissionOption, PermissionOptionKind, RequestPermissionOutcome,
    RequestPermissionRequest, SessionUpdate, StopReason, TextContent, ToolCall, ToolCallContent,
    ToolCallStatus, ToolCallUpdate, ToolCallUpdateFields,
};
const INITIALIZED: &str =
    r#"{"jsonrpc":"2.0","id":1,"result":{"protocolVersion":1,"agentCapabilities":{}}}"#;
const SESSION_CREATED: &str = r#"{"jsonrpc":"2.0","id":2,"result":{"sessionId":"session-1"}}"#;

fn ready_codec() -> AcpClientCodec {
    let mut codec = AcpClientCodec::new(MAX_FRAME_BYTES);
    codec.initialize().expect("initialize frame");
    codec
        .decode_frame(INITIALIZED)
        .expect("initialize response");
    codec
        .new_session(std::path::Path::new("/tmp"))
        .expect("new_session frame");
    codec
        .decode_frame(SESSION_CREATED)
        .expect("session/new response");
    codec
}

fn prompting_codec() -> AcpClientCodec {
    let mut codec = ready_codec();
    codec
        .prompt(vec![ContentBlock::Text(TextContent::new("hello"))])
        .expect("prompt frame");
    codec
}

#[test]
fn initialize_advertises_only_implemented_client_capabilities() {
    let mut codec = AcpClientCodec::new(MAX_FRAME_BYTES);
    let frame = codec.initialize().expect("initialize frame");
    let value: serde_json::Value = serde_json::from_str(&frame).expect("json frame");
    assert_eq!(value["method"], "initialize");
    assert_eq!(value["params"]["protocolVersion"], 1);
    assert_eq!(value["params"]["clientCapabilities"]["terminal"], false);
    assert_eq!(
        value["params"]["clientCapabilities"]["fs"]["readTextFile"],
        false
    );
    assert_eq!(
        value["params"]["clientCapabilities"]["fs"]["writeTextFile"],
        false
    );
    assert_eq!(value["params"]["clientInfo"]["name"], "cosh-shell");
    assert_eq!(codec.phase(), Phase::Initializing);
}

#[test]
fn host_shell_capability_carries_the_version_and_the_owned_pty_instance() {
    let mut codec = AcpClientCodec::new(MAX_FRAME_BYTES);
    let plain: serde_json::Value =
        serde_json::from_str(&codec.initialize().expect("initialize frame")).expect("json");
    assert!(plain["params"]["clientCapabilities"]["_meta"]["_cosh/shell"].is_null());

    let mut codec = AcpClientCodec::new(MAX_FRAME_BYTES);
    codec.enable_host_shell("pty-7".to_string());
    let frame = codec.initialize().expect("initialize frame");
    let value: serde_json::Value = serde_json::from_str(&frame).expect("json frame");
    let capability = &value["params"]["clientCapabilities"]["_meta"]["_cosh/shell"];
    assert_eq!(capability["version"], 1);
    assert_eq!(capability["ptyInstance"], "pty-7");
}

#[test]
fn initialize_rejects_unsupported_wire_version_and_stays_terminal() {
    let mut codec = AcpClientCodec::new(MAX_FRAME_BYTES);
    codec.initialize().expect("initialize frame");
    let failure = codec
        .decode_frame(r#"{"jsonrpc":"2.0","id":1,"result":{"protocolVersion":2}}"#)
        .expect_err("wire v2 must be rejected");
    assert!(failure.message.contains("unsupported ACP wire version 2"));
    assert_eq!(codec.phase(), Phase::Terminal);
    assert!(codec.decode_frame(INITIALIZED).is_err());
}

#[test]
fn out_of_order_requests_are_rejected_before_any_frame_is_sent() {
    let mut codec = AcpClientCodec::new(MAX_FRAME_BYTES);
    assert!(codec.new_session(std::path::Path::new("/tmp")).is_err());
    assert!(codec
        .prompt(vec![ContentBlock::Text(TextContent::new("hi"))])
        .is_err());
    assert!(codec.cancel().is_err());
    assert!(codec.decode_frame(INITIALIZED).is_err());
}

#[test]
fn session_new_requires_absolute_cwd_and_one_active_session() {
    let mut codec = AcpClientCodec::new(MAX_FRAME_BYTES);
    codec.initialize().expect("initialize frame");
    codec
        .decode_frame(INITIALIZED)
        .expect("initialize response");
    assert!(codec.new_session(std::path::Path::new("relative")).is_err());
    codec
        .new_session(std::path::Path::new("/tmp"))
        .expect("new_session frame");
    codec
        .decode_frame(SESSION_CREATED)
        .expect("session/new response");
    let failure = codec
        .new_session(std::path::Path::new("/tmp"))
        .expect_err("second session must be rejected");
    assert!(failure.message.contains("one active session"));
}

#[test]
fn prompt_requires_session_and_rejects_empty_content() {
    let mut codec = ready_codec();
    assert!(codec.prompt(Vec::new()).is_err());
    codec
        .prompt(vec![ContentBlock::Text(TextContent::new("hello"))])
        .expect("prompt frame");
    assert_eq!(codec.phase(), Phase::Prompting);
    assert!(codec
        .prompt(vec![ContentBlock::Text(TextContent::new("again"))])
        .is_err());
}

#[test]
fn cancel_notification_carries_the_bound_session() {
    let mut codec = prompting_codec();
    let frame = codec.cancel().expect("cancel frame");
    let value: serde_json::Value = serde_json::from_str(&frame).expect("json frame");
    assert_eq!(value["method"], "session/cancel");
    assert_eq!(value["params"]["sessionId"], "session-1");
    assert!(value.get("id").is_none());
}

#[test]
fn unknown_response_id_is_a_protocol_failure() {
    let mut codec = prompting_codec();
    let failure = codec
        .decode_frame(r#"{"jsonrpc":"2.0","id":99,"result":{"stopReason":"end_turn"}}"#)
        .expect_err("unknown id must be rejected");
    assert!(failure.message.contains("unknown id 99"));
    assert_eq!(codec.phase(), Phase::Terminal);
}

#[test]
fn update_for_a_foreign_session_is_rejected() {
    let mut codec = prompting_codec();
    let failure = codec
        .decode_frame(
            r#"{"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"other","update":{"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":"x"}}}}"#,
        )
        .expect_err("foreign session must be rejected");
    assert!(failure.message.contains("bound to session-1"));
}

#[test]
fn oversized_frame_is_rejected_before_decode() {
    let mut codec = AcpClientCodec::new(256);
    codec.initialize().expect("initialize frame");
    let oversized = format!(
        "{{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{{\"padding\":\"{}\"}}}}",
        "x".repeat(512)
    );
    let failure = codec
        .decode_frame(&oversized)
        .expect_err("oversized frame must be rejected");
    assert!(failure.message.contains("256 byte limit"));
    assert_eq!(codec.phase(), Phase::Terminal);
}

#[test]
fn batch_frame_is_rejected_as_malformed() {
    let mut codec = prompting_codec();
    let batch = r#"[{"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"session-1","update":{"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":"a"}}}},{"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"session-1","update":{"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":"b"}}}}]"#;
    // SDK 5.x classifies JSON-RPC batches as malformed; the codec fails closed
    // rather than decoding batch entries.
    let failure = codec
        .decode_frame(batch)
        .expect_err("batch frames must be rejected as malformed");
    assert!(failure.message.contains("malformed inbound JSON-RPC frame"));
}

#[test]
fn batch_rejection_terminates_the_codec() {
    let mut codec = prompting_codec();
    let entry = r#"{"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"session-1","update":{"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":"x"}}}}"#;
    let batch = format!("[{}]", vec![entry; 65].join(","));
    let _ = codec
        .decode_frame(&batch)
        .expect_err("batch frames must be rejected");
    let after = codec.decode_frame(entry);
    assert!(after
        .expect_err("codec must be terminal after a malformed batch")
        .message
        .contains("terminal"));
}

#[test]
fn stop_reason_ends_the_prompt_and_keeps_the_session() {
    let mut codec = prompting_codec();
    let decoded = codec
        .decode_frame(r#"{"jsonrpc":"2.0","id":3,"result":{"stopReason":"end_turn"}}"#)
        .expect("prompt response");
    assert_eq!(
        decoded.events,
        vec![Inbound::Stopped {
            reason: StopReason::EndTurn
        }]
    );
    assert_eq!(codec.phase(), Phase::Ready);
    assert_eq!(codec.session_id(), Some("session-1"));
}

#[test]
fn request_failure_reports_kind_and_code() {
    let mut codec = prompting_codec();
    let decoded = codec
        .decode_frame(
            r#"{"jsonrpc":"2.0","id":3,"error":{"code":-32603,"message":"agent exploded"}}"#,
        )
        .expect("error response");
    assert_eq!(
        decoded.events,
        vec![Inbound::Failed {
            kind: RequestKind::Prompt,
            code: -32603,
            message: "agent exploded".to_string(),
        }]
    );
}

#[test]
fn permission_reverse_call_is_surfaced_and_answered_with_a_known_option() {
    let mut codec = prompting_codec();
    let decoded = codec
        .decode_frame(
            r#"{"jsonrpc":"2.0","id":7,"method":"session/request_permission","params":{"sessionId":"session-1","toolCall":{"toolCallId":"tool-1"},"options":[{"optionId":"allow","name":"Allow","kind":"allow_once"},{"optionId":"reject","name":"Reject","kind":"reject_once"}]}}"#,
        )
        .expect("permission request");
    let Inbound::Permission { id, request } = &decoded.events[0] else {
        panic!("expected a permission fact, got {:?}", decoded.events);
    };
    let outcome = permission_outcome(request, reject_by_default(request));
    assert_eq!(
        outcome,
        RequestPermissionOutcome::Selected(
            agent_client_protocol::v1::schema::SelectedPermissionOutcome::new("reject")
        )
    );
    let reply = codec
        .permission_reply(id.clone(), outcome)
        .expect("permission reply");
    let value: serde_json::Value = serde_json::from_str(&reply).expect("json reply");
    assert_eq!(value["id"], 7);
    assert_eq!(value["result"]["outcome"]["outcome"], "selected");
    assert_eq!(value["result"]["outcome"]["optionId"], "reject");
}

#[test]
fn unknown_permission_option_never_becomes_consent() {
    let request = RequestPermissionRequest::new(
        "session-1",
        ToolCallUpdate::new("tool-1", ToolCallUpdateFields::default()),
        vec![PermissionOption::new(
            "allow",
            "Allow",
            PermissionOptionKind::AllowOnce,
        )],
    );
    assert_eq!(
        permission_outcome(
            &request,
            AcpPermissionDecision::Select("not-offered".to_string())
        ),
        RequestPermissionOutcome::Cancelled
    );
    assert_eq!(
        permission_outcome(&request, reject_by_default(&request)),
        RequestPermissionOutcome::Cancelled
    );
}

#[test]
fn unsupported_reverse_method_gets_an_explicit_method_not_found() {
    let mut codec = prompting_codec();
    let decoded = codec
        .decode_frame(
            r#"{"jsonrpc":"2.0","id":8,"method":"fs/read_text_file","params":{"sessionId":"session-1","path":"/etc/passwd"}}"#,
        )
        .expect("unsupported request");
    let Inbound::UnsupportedRequest { id, method } = &decoded.events[0] else {
        panic!("expected an unsupported-request fact");
    };
    assert_eq!(method, "fs/read_text_file");
    let reply = codec
        .unsupported_reply(id.clone(), method)
        .expect("method-not-found reply");
    let value: serde_json::Value = serde_json::from_str(&reply).expect("json reply");
    assert_eq!(value["id"], 8);
    assert_eq!(value["error"]["code"], -32601);
}

#[test]
fn stdout_close_produces_one_terminal_fact() {
    let mut codec = prompting_codec();
    assert_eq!(codec.finish_stdout(), Some(Inbound::TransportClosed));
    assert_eq!(codec.finish_stdout(), None);
}

#[test]
fn projection_maps_text_thought_and_tool_lifecycle() {
    let text = SessionUpdate::AgentMessageChunk(ContentChunk::new(ContentBlock::Text(
        TextContent::new("answer"),
    )));
    assert_eq!(
        project_update("run-1", text),
        vec![crate::types::AgentEvent::TextDelta {
            run_id: "run-1".to_string(),
            text: "answer".to_string(),
        }]
    );

    let thought = SessionUpdate::AgentThoughtChunk(ContentChunk::new(ContentBlock::Text(
        TextContent::new("reasoning"),
    )));
    let events = project_update("run-1", thought);
    assert!(matches!(
        &events[0],
        crate::types::AgentEvent::StatusChanged { phase, .. } if phase == "thinking"
    ));

    let mut call = ToolCall::new("tool-1", "Read file");
    call.status = ToolCallStatus::Completed;
    call.content = vec![ToolCallContent::from(ContentBlock::Text(TextContent::new(
        "file body",
    )))];
    let events = project_update("run-1", SessionUpdate::ToolCall(call));
    assert!(matches!(
        &events[0],
        crate::types::AgentEvent::ToolCall { tool_id: Some(id), name, .. }
            if id == "tool-1" && name == "Read file"
    ));
    assert!(events.iter().any(|event| matches!(
        event,
        crate::types::AgentEvent::ToolOutputDelta { text, stream, .. }
            if text == "file body" && stream == "stdout"
    )));
    assert!(events.iter().any(|event| matches!(
        event,
        crate::types::AgentEvent::ToolCompleted { status, .. } if status == "success"
    )));
}

#[test]
fn projection_reports_unrenderable_content_instead_of_dropping_it() {
    let update = SessionUpdate::AgentMessageChunk(ContentChunk::new(ContentBlock::Image(
        agent_client_protocol::v1::schema::ImageContent::new("base64", "image/png"),
    )));
    let events = project_update("run-1", update);
    assert!(matches!(
        &events[0],
        crate::types::AgentEvent::StatusChanged { phase, message, .. }
            if phase == "unsupported_content" && message.contains("image")
    ));

    let mut fields = ToolCallUpdateFields::default();
    fields.status = Some(ToolCallStatus::Failed);
    fields.content = Some(vec![ToolCallContent::Terminal(
        agent_client_protocol::v1::schema::Terminal::new("terminal-1"),
    )]);
    let events = project_update(
        "run-1",
        SessionUpdate::ToolCallUpdate(ToolCallUpdate::new("tool-1", fields)),
    );
    assert!(events.iter().any(|event| matches!(
        event,
        crate::types::AgentEvent::ToolOutputDelta { text, stream, .. }
            if stream == "stderr" && text.contains("terminal/*")
    )));
    assert!(events.iter().any(|event| matches!(
        event,
        crate::types::AgentEvent::ToolCompleted { status, .. } if status == "error"
    )));
}

#[test]
fn projection_summarizes_plan_with_a_bound() {
    let entries = (0..20)
        .map(|index| {
            agent_client_protocol::v1::schema::PlanEntry::new(
                format!("step {index}"),
                agent_client_protocol::v1::schema::PlanEntryPriority::Medium,
                agent_client_protocol::v1::schema::PlanEntryStatus::Pending,
            )
        })
        .collect();
    let events = project_update(
        "run-1",
        SessionUpdate::Plan(agent_client_protocol::v1::schema::Plan::new(entries)),
    );
    let crate::types::AgentEvent::StatusChanged { phase, message, .. } = &events[0] else {
        panic!("expected a plan status");
    };
    assert_eq!(phase, "plan");
    assert!(message.contains("20 entries"));
    assert!(message.contains("(+4 more)"));
    assert!(!message.contains("step 19"));
}

#[test]
fn outbound_prompt_frame_is_bounded() {
    let mut codec = AcpClientCodec::new(256);
    codec.initialize().expect("initialize frame");
    codec
        .decode_frame(INITIALIZED)
        .expect("initialize response");
    codec
        .new_session(std::path::Path::new("/tmp"))
        .expect("new_session frame");
    codec
        .decode_frame(SESSION_CREATED)
        .expect("session/new response");
    let failure = codec
        .prompt(vec![ContentBlock::Text(TextContent::new("x".repeat(512)))])
        .expect_err("oversized prompt must be rejected");
    assert!(failure.message.contains("256 byte limit"));
    assert_eq!(codec.phase(), Phase::Ready);
}

#[test]
fn malformed_frame_is_terminal() {
    let mut codec = prompting_codec();
    assert!(codec.decode_frame("{not json").is_err());
    assert_eq!(codec.phase(), Phase::Terminal);
}

#[test]
fn legacy_stream_parser_does_not_project_acp_frames() {
    let mut parser =
        super::super::claude_stream::ClaudeStreamParser::new("run-legacy".into(), None);
    let frames = [
        INITIALIZED,
        SESSION_CREATED,
        r#"{"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"session-1","update":{"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":"acp text"}}}}"#,
        r#"{"jsonrpc":"2.0","id":3,"result":{"stopReason":"end_turn"}}"#,
    ];
    for frame in frames {
        assert!(
            parser.parse_line(frame).is_empty(),
            "the legacy parser must not turn an ACP frame into a fact: {frame}"
        );
    }
}

#[test]
fn acp_entry_detach_is_idempotent_without_a_kept_peer() {
    let adapter = super::AcpAgentAdapter::new("acp-agent");
    assert_eq!(
        super::super::AdapterInstance::Acp(adapter).start_fresh_session(),
        super::super::FreshSessionOutcome::Detached {
            previous_session_id: None
        }
    );
}

#[test]
fn the_frozen_shared_contract_still_matches_the_legacy_builder() {
    let legacy = super::super::prompt::provider_prompt_contract_for_language(
        crate::types::CoshApprovalMode::Auto,
        "shell",
        crate::Language::EnUs,
    );
    let frozen = include_str!("../../../tests/protocol/acp_contract_reference.txt");
    let mut checked = 0;
    for line in frozen.lines() {
        assert!(
            legacy.contains(line),
            "the frozen shared contract drifted from the legacy builder: {line:?}"
        );
        checked += 1;
    }
    assert!(
        checked >= 8,
        "the shared invariant set is too small: {checked}"
    );
}
