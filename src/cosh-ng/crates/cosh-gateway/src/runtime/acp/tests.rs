//! Focused ACP v1 codec and supervised stdio bridge tests.

use std::path::PathBuf;
use std::time::Duration;

use serde_json::{json, Value};

use super::*;
use crate::runtime::{RuntimeLaunchSpec, RuntimeState};

const FRAME_LIMIT: usize = 16 * 1024;
const SESSION_ID: &str = "agent-session-1";

fn codec() -> AcpV1Codec {
    AcpV1Codec::new(AcpV1ClientConfig::new("cosh-ng", "0.15.0", FRAME_LIMIT)).unwrap()
}

fn codex_codec() -> AcpV1Codec {
    AcpV1Codec::new(
        AcpV1ClientConfig::new("cosh-ng", "0.15.0", FRAME_LIMIT)
            .adapter_profile(AcpV1AdapterProfile::Codex162),
    )
    .unwrap()
}

fn initialize_codex(codec: &mut AcpV1Codec) -> AcpV1Observation {
    let request: Value = serde_json::from_str(&codec.initialize_frame().unwrap()).unwrap();
    assert_eq!(
        request["params"]["clientCapabilities"]["_meta"],
        json!({
            "jetbrains": {
                "air": {"version": 1, "capabilities": ["sessionFailure"]}
            }
        })
    );
    let response = codex_initialize_response(
        "@agentclientprotocol/codex-acp",
        "1.6.2",
        json!({"version": 1, "capabilities": ["sessionFailure", "agentFileChangeReport"]}),
    );
    codec.decode_frame(response.to_string().as_bytes()).unwrap()
}

fn codex_initialize_response(name: &str, version: &str, air: Value) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": "cosh-acp-1",
        "result": {
            "protocolVersion": 1,
            "agentCapabilities": {},
            "agentInfo": {"name": name, "title": "Codex", "version": version},
            "_meta": {"jetbrains": {"air": air}}
        }
    })
}

fn session_failure(severity: &str) -> Value {
    json!({
        "id": "turn-1:error",
        "revision": 1,
        "category": "service",
        "severity": severity,
        "title": "Codex provider failed",
        "actions": ["retry"]
    })
}

fn initialize(codec: &mut AcpV1Codec, capabilities: Value) -> AcpV1Observation {
    let request = codec.initialize_frame().unwrap();
    let value: Value = serde_json::from_str(&request).unwrap();
    assert_eq!(value["jsonrpc"], "2.0");
    assert_eq!(value["id"], "cosh-acp-1");
    assert_eq!(value["method"], "initialize");
    assert_eq!(value["params"]["protocolVersion"], 1);
    assert_eq!(value["params"]["clientInfo"]["name"], "cosh-ng");
    assert_eq!(value["params"]["clientInfo"]["version"], "0.15.0");

    let response = json!({
        "jsonrpc": "2.0",
        "id": "cosh-acp-1",
        "result": {
            "protocolVersion": 1,
            "agentCapabilities": capabilities,
            "agentInfo": {
                "name": "fake-agent",
                "title": "Fake Agent",
                "version": "1.2.3"
            }
        }
    });
    codec.decode_frame(response.to_string().as_bytes()).unwrap()
}

fn open_session(codec: &mut AcpV1Codec) {
    let frame = codec
        .new_session_frame(PathBuf::from("/workspace"), Vec::new())
        .unwrap();
    let value: Value = serde_json::from_str(&frame).unwrap();
    assert_eq!(value["id"], "cosh-acp-2");
    assert_eq!(value["method"], "session/new");
    assert_eq!(value["params"]["cwd"], "/workspace");

    let response = json!({
        "jsonrpc": "2.0",
        "id": "cosh-acp-2",
        "result": { "sessionId": SESSION_ID }
    });
    assert_eq!(
        codec.decode_frame(response.to_string().as_bytes()).unwrap(),
        AcpV1Observation::SessionOpened {
            session_id: SESSION_ID.to_owned()
        }
    );
}

fn start_prompt(codec: &mut AcpV1Codec) -> Value {
    let frame = codec.prompt_frame("inspect this workspace").unwrap();
    serde_json::from_str(&frame).unwrap()
}

#[test]
fn initialization_pins_wire_v1_independently_from_sdk_version() {
    let mut codec = codec();
    let observation = initialize(
        &mut codec,
        json!({
            "loadSession": true,
            "promptCapabilities": {
                "image": true,
                "embeddedContext": true
            },
            "sessionCapabilities": {
                "additionalDirectories": {},
                "resume": {},
                "close": {}
            }
        }),
    );

    assert_eq!(codec.phase(), AcpV1ProtocolPhase::Ready);
    assert_eq!(
        observation,
        AcpV1Observation::Initialized {
            agent_info: Some(AcpV1AgentInfo {
                name: "fake-agent".to_owned(),
                title: Some("Fake Agent".to_owned()),
                version: "1.2.3".to_owned(),
            }),
            capabilities: AcpV1AgentCapabilities {
                load_session: true,
                additional_directories: true,
                resume_session: true,
                close_session: true,
                image_prompts: true,
                embedded_context: true,
                ..AcpV1AgentCapabilities::default()
            }
        }
    );
    assert_eq!(ACP_WIRE_PROTOCOL_VERSION, 1);
}

#[test]
fn codex_profile_negotiates_only_the_frozen_air_v1_contract() {
    let mut codec = codex_codec();
    let observation = initialize_codex(&mut codec);

    assert!(matches!(
        observation,
        AcpV1Observation::Initialized {
            agent_info: Some(AcpV1AgentInfo { ref name, ref version, .. }),
            ..
        } if name == "@agentclientprotocol/codex-acp" && version == "1.6.2"
    ));
    assert_eq!(codec.phase(), AcpV1ProtocolPhase::Ready);
}

#[test]
fn codex_profile_rejects_unfrozen_identity_before_session_authority() {
    for (name, version) in [
        ("@agentclientprotocol/codex-acp", "1.2.0"),
        ("@agentclientprotocol/codex-acp", "1.6.3"),
        ("codex-acp", "1.6.2"),
    ] {
        let mut codec = codex_codec();
        codec.initialize_frame().unwrap();
        let response = codex_initialize_response(
            name,
            version,
            json!({"version": 1, "capabilities": ["sessionFailure", "agentFileChangeReport"]}),
        );
        assert!(matches!(
            codec.decode_frame(response.to_string().as_bytes()),
            Err(AcpV1CodecError::CodexAdapterIdentityMismatch { .. })
        ));
        assert_eq!(codec.phase(), AcpV1ProtocolPhase::Terminal);
    }

    let mut missing = codex_codec();
    missing.initialize_frame().unwrap();
    let response = json!({
        "jsonrpc": "2.0",
        "id": "cosh-acp-1",
        "result": {
            "protocolVersion": 1,
            "agentCapabilities": {},
            "_meta": {"jetbrains": {"air": {
                "version": 1,
                "capabilities": ["sessionFailure", "agentFileChangeReport"]
            }}}
        }
    });
    assert!(matches!(
        missing.decode_frame(response.to_string().as_bytes()),
        Err(AcpV1CodecError::CodexAdapterIdentityMismatch {
            name: None,
            version: None
        })
    ));
    assert_eq!(missing.phase(), AcpV1ProtocolPhase::Terminal);
}

#[test]
fn codex_profile_rejects_missing_or_drifted_air_negotiation() {
    for air in [
        json!({}),
        json!({"version": 2, "capabilities": ["sessionFailure", "agentFileChangeReport"]}),
        json!({"version": 1, "capabilities": ["sessionFailure"]}),
        json!({"version": 1, "capabilities": ["agentFileChangeReport", "sessionFailure"]}),
    ] {
        let mut codec = codex_codec();
        codec.initialize_frame().unwrap();
        let response = codex_initialize_response("@agentclientprotocol/codex-acp", "1.6.2", air);
        assert!(matches!(
            codec.decode_frame(response.to_string().as_bytes()),
            Err(AcpV1CodecError::InvalidCodexSessionFailureNegotiation(_))
        ));
        assert_eq!(codec.phase(), AcpV1ProtocolPhase::Terminal);
    }
}

#[test]
fn codex_partial_text_then_typed_error_is_request_failure() {
    let mut codec = codex_codec();
    initialize_codex(&mut codec);
    open_session(&mut codec);
    start_prompt(&mut codec);
    let partial = json!({
        "jsonrpc": "2.0",
        "method": "session/update",
        "params": {
            "sessionId": SESSION_ID,
            "update": {
                "sessionUpdate": "agent_message_chunk",
                "content": {"type": "text", "text": "partial answer"}
            }
        }
    });
    assert!(matches!(
        codec.decode_frame(partial.to_string().as_bytes()),
        Ok(AcpV1Observation::SessionUpdate { .. })
    ));
    let response = json!({
        "jsonrpc": "2.0",
        "id": "cosh-acp-3",
        "result": {
            "stopReason": "end_turn",
            "_meta": {
                "quota": {"token_count": null, "model_usage": []},
                "jetbrains": {"air": {"version": 1, "sessionFailure": session_failure("error")}}
            }
        }
    });

    assert_eq!(
        codec.decode_frame(response.to_string().as_bytes()).unwrap(),
        AcpV1Observation::RequestFailed {
            request: AcpV1RequestKind::Prompt,
            code: -32000,
            message: "Codex provider failed".to_owned(),
        }
    );
}

#[test]
fn codex_warning_update_then_clean_end_turn_succeeds() {
    let mut codec = codex_codec();
    initialize_codex(&mut codec);
    open_session(&mut codec);
    start_prompt(&mut codec);
    let warning = json!({
        "jsonrpc": "2.0",
        "method": "session/update",
        "params": {
            "sessionId": SESSION_ID,
            "update": {
                "sessionUpdate": "session_info_update",
                "_meta": {
                    "jetbrains": {"air": {"version": 1, "sessionFailure": session_failure("warning")}}
                }
            }
        }
    });
    assert!(matches!(
        codec.decode_frame(warning.to_string().as_bytes()),
        Ok(AcpV1Observation::SessionUpdate { .. })
    ));
    let response = json!({
        "jsonrpc": "2.0",
        "id": "cosh-acp-3",
        "result": {"stopReason": "end_turn", "_meta": {"quota": {}}}
    });
    assert!(matches!(
        codec.decode_frame(response.to_string().as_bytes()),
        Ok(AcpV1Observation::PromptFinished {
            stop_reason: AcpV1StopReason::EndTurn,
            ..
        })
    ));
}

#[test]
fn codex_error_update_fails_closed_before_clean_terminal_response() {
    let mut codec = codex_codec();
    initialize_codex(&mut codec);
    open_session(&mut codec);
    start_prompt(&mut codec);
    let error = json!({
        "jsonrpc": "2.0",
        "method": "session/update",
        "params": {
            "sessionId": SESSION_ID,
            "update": {
                "sessionUpdate": "session_info_update",
                "_meta": {
                    "jetbrains": {"air": {"version": 1, "sessionFailure": session_failure("error")}}
                }
            }
        }
    });

    assert!(matches!(
        codec.decode_frame(error.to_string().as_bytes()),
        Err(AcpV1CodecError::InvalidCodexSessionFailure(
            "error used a non-terminal session_info_update carrier"
        ))
    ));
    assert_eq!(codec.phase(), AcpV1ProtocolPhase::Terminal);
}

#[test]
fn codex_warning_requires_session_info_update_carrier() {
    let mut codec = codex_codec();
    initialize_codex(&mut codec);
    open_session(&mut codec);
    start_prompt(&mut codec);
    let wrong_carrier = json!({
        "jsonrpc": "2.0",
        "method": "session/update",
        "params": {
            "sessionId": SESSION_ID,
            "update": {
                "sessionUpdate": "agent_message_chunk",
                "content": {"type": "text", "text": "retrying"},
                "_meta": {
                    "jetbrains": {"air": {"version": 1, "sessionFailure": session_failure("warning")}}
                }
            }
        }
    });

    assert!(matches!(
        codec.decode_frame(wrong_carrier.to_string().as_bytes()),
        Err(AcpV1CodecError::InvalidCodexSessionFailure(
            "failure notification used a non-session_info_update carrier"
        ))
    ));
    assert_eq!(codec.phase(), AcpV1ProtocolPhase::Terminal);
}

#[test]
fn codex_session_failure_schema_drift_fails_closed() {
    let mut malformed_failures = vec![
        json!({
            "id": "turn-1:error", "revision": 1, "category": "future",
            "severity": "error", "title": "failed", "actions": []
        }),
        json!({
            "id": "turn-1:error", "revision": 1, "category": "service",
            "severity": "fatal", "title": "failed", "actions": []
        }),
        json!({
            "id": "turn-1:error", "revision": 0, "category": "service",
            "severity": "error", "title": "failed", "actions": []
        }),
        json!({
            "id": "turn-1:error", "revision": 1, "category": "service",
            "severity": "error", "actions": []
        }),
        json!({
            "id": "turn-1:error", "revision": 1, "category": "service",
            "severity": "error", "title": "failed", "actions": [], "source": "provider"
        }),
        json!({
            "id": "turn-1:error", "revision": 1, "category": "service",
            "severity": "error", "title": "failed", "actions": ["future"]
        }),
        json!({
            "id": "x".repeat(1025), "revision": 1, "category": "service",
            "severity": "error", "title": "failed", "actions": []
        }),
    ];
    malformed_failures.push(
        serde_json::from_str(
            r#"{"id":"turn-1:error","revision":18446744073709551616,"category":"service","severity":"error","title":"failed","actions":[]}"#,
        )
        .unwrap(),
    );
    for failure in malformed_failures {
        let mut codec = codex_codec();
        initialize_codex(&mut codec);
        open_session(&mut codec);
        start_prompt(&mut codec);
        let response = json!({
            "jsonrpc": "2.0",
            "id": "cosh-acp-3",
            "result": {
                "stopReason": "end_turn",
                "_meta": {"jetbrains": {"air": {"version": 1, "sessionFailure": failure}}}
            }
        });
        assert!(matches!(
            codec.decode_frame(response.to_string().as_bytes()),
            Err(AcpV1CodecError::InvalidCodexSessionFailure(_))
        ));
        assert_eq!(codec.phase(), AcpV1ProtocolPhase::Terminal);
    }
}

#[test]
fn codex_session_failure_outer_drift_fails_closed() {
    for meta in [
        json!({"jetbrains": {"sessionFailure": session_failure("error")}}),
        json!({"jetbrains": {"air": {
            "version": 2, "sessionFailure": session_failure("error")
        }}}),
        json!({"jetbrains": {"air": {
            "version": 1, "sessionFailure": session_failure("error"), "extra": true
        }}}),
    ] {
        let mut codec = codex_codec();
        initialize_codex(&mut codec);
        open_session(&mut codec);
        start_prompt(&mut codec);
        let response = json!({
            "jsonrpc": "2.0",
            "id": "cosh-acp-3",
            "result": {"stopReason": "end_turn", "_meta": meta}
        });

        assert!(matches!(
            codec.decode_frame(response.to_string().as_bytes()),
            Err(AcpV1CodecError::InvalidCodexSessionFailure(_))
        ));
        assert_eq!(codec.phase(), AcpV1ProtocolPhase::Terminal);
    }
}

#[test]
fn wrong_protocol_version_fails_closed() {
    let mut codec = codec();
    codec.initialize_frame().unwrap();
    let response = json!({
        "jsonrpc": "2.0",
        "id": "cosh-acp-1",
        "result": { "protocolVersion": 2 }
    });

    assert!(matches!(
        codec.decode_frame(response.to_string().as_bytes()),
        Err(AcpV1CodecError::UnsupportedProtocolVersion { actual: 2 })
    ));
    assert_eq!(codec.phase(), AcpV1ProtocolPhase::Terminal);
}

#[test]
fn session_requires_absolute_and_advertised_additional_roots() {
    let mut codec = codec();
    initialize(&mut codec, json!({}));

    assert!(matches!(
        codec.new_session_frame("relative", Vec::new()),
        Err(AcpV1CodecError::WorkspaceNotAbsolute(_))
    ));
    assert!(matches!(
        codec.new_session_frame("/workspace", vec![PathBuf::from("/workspace-secondary")]),
        Err(AcpV1CodecError::UnsupportedCapability(
            "session.additionalDirectories"
        ))
    ));
}

#[test]
fn prompt_update_and_terminal_response_preserve_session_binding() {
    let mut codec = codec();
    initialize(&mut codec, json!({}));
    open_session(&mut codec);

    let prompt = start_prompt(&mut codec);
    assert_eq!(prompt["id"], "cosh-acp-3");
    assert_eq!(prompt["method"], "session/prompt");
    assert_eq!(prompt["params"]["sessionId"], SESSION_ID);
    assert_eq!(prompt["params"]["prompt"][0]["type"], "text");
    assert_eq!(
        prompt["params"]["prompt"][0]["text"],
        "inspect this workspace"
    );

    let update = json!({
        "jsonrpc": "2.0",
        "method": "session/update",
        "params": {
            "sessionId": SESSION_ID,
            "update": {
                "sessionUpdate": "agent_message_chunk",
                "content": { "type": "text", "text": "working" },
                "messageId": "message-1"
            }
        }
    });
    let observation = codec.decode_frame(update.to_string().as_bytes()).unwrap();
    let AcpV1Observation::SessionUpdate { session_id, update } = observation else {
        panic!("expected session update");
    };
    assert_eq!(session_id, SESSION_ID);
    assert_eq!(update["sessionUpdate"], "agent_message_chunk");
    assert_eq!(update["content"]["text"], "working");

    let result = json!({
        "jsonrpc": "2.0",
        "id": "cosh-acp-3",
        "result": { "stopReason": "end_turn" }
    });
    assert_eq!(
        codec.decode_frame(result.to_string().as_bytes()).unwrap(),
        AcpV1Observation::PromptFinished {
            session_id: SESSION_ID.to_owned(),
            stop_reason: AcpV1StopReason::EndTurn,
        }
    );
}

#[test]
fn mixed_batch_preserves_observation_order() {
    let mut codec = codec();
    initialize(&mut codec, json!({}));
    open_session(&mut codec);
    start_prompt(&mut codec);
    let batch = json!([
        {
            "jsonrpc": "2.0",
            "method": "session/update",
            "params": {
                "sessionId": SESSION_ID,
                "update": {
                    "sessionUpdate": "agent_message_chunk",
                    "content": { "type": "text", "text": "batched" }
                }
            }
        },
        {
            "jsonrpc": "2.0",
            "id": "cosh-acp-3",
            "result": { "stopReason": "end_turn" }
        }
    ]);

    let decoded = codec
        .decode_transport_frame(batch.to_string().as_bytes())
        .unwrap();
    assert!(decoded.outbound_frames.is_empty());
    assert!(matches!(
        decoded.observations.as_slice(),
        [
            AcpV1Observation::SessionUpdate { .. },
            AcpV1Observation::PromptFinished {
                stop_reason: AcpV1StopReason::EndTurn,
                ..
            }
        ]
    ));
}

#[test]
fn batch_errors_notifications_and_requests_are_independent() {
    let mut codec = codec();
    initialize(&mut codec, json!({}));
    open_session(&mut codec);
    start_prompt(&mut codec);
    let batch = json!([
        17,
        {
            "jsonrpc": "2.0",
            "id": 91,
            "method": "fs/read_text_file",
            "params": { "sessionId": SESSION_ID, "path": "/tmp/input" }
        },
        {
            "jsonrpc": "2.0",
            "method": "extension/progress",
            "params": { "percent": 50 }
        }
    ]);

    let decoded = codec
        .decode_transport_frame(batch.to_string().as_bytes())
        .unwrap();
    assert!(decoded.outbound_frames.is_empty());
    assert!(matches!(
        decoded.observations.as_slice(),
        [
            AcpV1Observation::UnsupportedClientRequest { .. },
            AcpV1Observation::UnsupportedNotification { .. }
        ]
    ));

    let frames = codec
        .reject_unsupported_request_frames(&AcpV1RequestId::Number(91))
        .unwrap();
    assert_eq!(frames.len(), 1);
    let response: Value = serde_json::from_str(&frames[0]).unwrap();
    let responses = response.as_array().unwrap();
    assert_eq!(responses.len(), 2);
    assert_eq!(responses[0]["id"], Value::Null);
    assert_eq!(responses[0]["error"]["code"], -32600);
    assert_eq!(responses[1]["id"], 91);
    assert_eq!(responses[1]["error"]["code"], -32601);
}

#[test]
fn empty_batch_gets_invalid_request_and_keeps_connection_ready() {
    let mut codec = codec();
    initialize(&mut codec, json!({}));

    let decoded = codec.decode_transport_frame(b"[]").unwrap();
    assert!(decoded.observations.is_empty());
    assert_eq!(decoded.outbound_frames.len(), 1);
    let response: Value = serde_json::from_str(&decoded.outbound_frames[0]).unwrap();
    assert_eq!(response["id"], Value::Null);
    assert_eq!(response["error"]["code"], -32600);
    assert_eq!(codec.phase(), AcpV1ProtocolPhase::Ready);
}

#[test]
fn batch_entry_count_is_bounded_independently_from_frame_bytes() {
    let mut codec =
        AcpV1Codec::new(AcpV1ClientConfig::new("cosh-ng", "0.15.0", 1024 * 1024)).unwrap();
    initialize(&mut codec, json!({}));
    let entries = (0..1025)
        .map(|index| {
            json!({
                "jsonrpc": "2.0",
                "method": "extension/progress",
                "params": { "index": index }
            })
        })
        .collect::<Vec<_>>();
    let batch = serde_json::to_vec(&entries).unwrap();

    assert!(matches!(
        codec.decode_transport_frame(&batch),
        Err(AcpV1CodecError::BatchTooLarge { limit: 1024 })
    ));
    assert_eq!(codec.phase(), AcpV1ProtocolPhase::Terminal);
}

#[test]
fn error_response_in_batch_is_correlated_independently() {
    let mut codec = codec();
    initialize(&mut codec, json!({}));
    open_session(&mut codec);
    start_prompt(&mut codec);
    let batch = json!([
        {
            "jsonrpc": "2.0",
            "method": "extension/progress",
            "params": { "percent": 50 }
        },
        {
            "jsonrpc": "2.0",
            "id": "cosh-acp-3",
            "error": { "code": -32000, "message": "provider unavailable" }
        }
    ]);

    let decoded = codec
        .decode_transport_frame(batch.to_string().as_bytes())
        .unwrap();
    assert!(matches!(
        decoded.observations.as_slice(),
        [
            AcpV1Observation::UnsupportedNotification { .. },
            AcpV1Observation::RequestFailed {
                request: AcpV1RequestKind::Prompt,
                code: -32000,
                ..
            }
        ]
    ));
}

#[test]
fn update_for_another_session_fails_closed() {
    let mut codec = codec();
    initialize(&mut codec, json!({}));
    open_session(&mut codec);
    start_prompt(&mut codec);
    let update = json!({
        "jsonrpc": "2.0",
        "method": "session/update",
        "params": {
            "sessionId": "spoofed-session",
            "update": {
                "sessionUpdate": "agent_message_chunk",
                "content": { "type": "text", "text": "spoofed" }
            }
        }
    });

    assert!(matches!(
        codec.decode_frame(update.to_string().as_bytes()),
        Err(AcpV1CodecError::SessionMismatch { .. })
    ));
    assert_eq!(codec.phase(), AcpV1ProtocolPhase::Terminal);
}

#[test]
fn update_without_an_active_prompt_fails_closed() {
    let mut codec = codec();
    initialize(&mut codec, json!({}));
    open_session(&mut codec);
    let update = json!({
        "jsonrpc": "2.0",
        "method": "session/update",
        "params": {
            "sessionId": SESSION_ID,
            "update": {
                "sessionUpdate": "agent_message_chunk",
                "content": { "type": "text", "text": "late" }
            }
        }
    });

    assert!(matches!(
        codec.decode_frame(update.to_string().as_bytes()),
        Err(AcpV1CodecError::PromptNotActive)
    ));
}

fn permission_request(id: Value) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "method": "session/request_permission",
        "params": {
            "sessionId": SESSION_ID,
            "toolCall": {
                "toolCallId": "tool-1",
                "title": "Run diagnostics"
            },
            "options": [
                {
                    "optionId": "allow-once",
                    "name": "Allow once",
                    "kind": "allow_once"
                },
                {
                    "optionId": "reject-once",
                    "name": "Reject once",
                    "kind": "reject_once"
                }
            ]
        }
    })
}

#[test]
fn permission_response_is_bound_to_offered_option() {
    let mut codec = codec();
    initialize(&mut codec, json!({}));
    open_session(&mut codec);
    start_prompt(&mut codec);
    let observation = codec
        .decode_frame(permission_request(json!(41)).to_string().as_bytes())
        .unwrap();
    let AcpV1Observation::PermissionRequested(request) = observation else {
        panic!("expected permission request");
    };
    assert_eq!(request.request_id, AcpV1RequestId::Number(41));
    assert_eq!(request.session_id, SESSION_ID);
    assert_eq!(request.options.len(), 2);

    assert!(matches!(
        codec.permission_response_frames(
            &request.request_id,
            AcpV1PermissionDecision::Selected {
                option_id: "allow-always".to_owned()
            }
        ),
        Err(AcpV1CodecError::UnknownPermissionOption { .. })
    ));
    let frames = codec
        .permission_response_frames(
            &request.request_id,
            AcpV1PermissionDecision::Selected {
                option_id: "allow-once".to_owned(),
            },
        )
        .unwrap();
    assert_eq!(frames.len(), 1);
    let frame = &frames[0];
    let value: Value = serde_json::from_str(frame).unwrap();
    assert_eq!(value["id"], 41);
    assert_eq!(value["result"]["outcome"]["outcome"], "selected");
    assert_eq!(value["result"]["outcome"]["optionId"], "allow-once");
}

#[test]
fn callback_digest_binds_extension_metadata_and_request_id_type() {
    fn decode_callback(id: Value, marker: &str) -> AcpV1PermissionRequest {
        let mut codec = codec();
        initialize(&mut codec, json!({}));
        open_session(&mut codec);
        start_prompt(&mut codec);
        let mut callback = permission_request(id);
        callback["params"]["_meta"] = json!({
            "codex": {"permission": {"marker": marker}}
        });
        let AcpV1Observation::PermissionRequested(request) =
            codec.decode_frame(callback.to_string().as_bytes()).unwrap()
        else {
            panic!("expected permission request");
        };
        request
    }

    let first = decode_callback(json!(1), "first");
    let different_meta = decode_callback(json!(1), "second");
    let string_id = decode_callback(json!("1"), "first");
    assert_ne!(
        first.callback_payload_digest,
        different_meta.callback_payload_digest
    );
    assert_ne!(
        first.callback_payload_digest,
        string_id.callback_payload_digest
    );
}

#[test]
fn batched_permission_responses_are_emitted_once_in_source_order() {
    let mut codec = codec();
    initialize(&mut codec, json!({}));
    open_session(&mut codec);
    start_prompt(&mut codec);
    let batch = json!([permission_request(json!(41)), permission_request(json!(42))]);
    let decoded = codec
        .decode_transport_frame(batch.to_string().as_bytes())
        .unwrap();
    assert_eq!(decoded.observations.len(), 2);
    assert!(decoded.outbound_frames.is_empty());

    let first = codec
        .permission_response_frames(
            &AcpV1RequestId::Number(41),
            AcpV1PermissionDecision::Selected {
                option_id: "allow-once".to_owned(),
            },
        )
        .unwrap();
    assert!(first.is_empty());
    let second = codec
        .permission_response_frames(
            &AcpV1RequestId::Number(42),
            AcpV1PermissionDecision::Selected {
                option_id: "reject-once".to_owned(),
            },
        )
        .unwrap();
    assert_eq!(second.len(), 1);
    let responses: Value = serde_json::from_str(&second[0]).unwrap();
    let responses = responses.as_array().unwrap();
    assert_eq!(responses[0]["id"], 41);
    assert_eq!(responses[1]["id"], 42);
    assert_eq!(responses[0]["result"]["outcome"]["optionId"], "allow-once");
    assert_eq!(responses[1]["result"]["outcome"]["optionId"], "reject-once");
}

#[test]
fn durable_permission_options_cannot_cross_the_mvp_proxy() {
    let mut codec = codec();
    initialize(&mut codec, json!({}));
    open_session(&mut codec);
    start_prompt(&mut codec);
    let callback = json!({
        "jsonrpc": "2.0",
        "id": 42,
        "method": "session/request_permission",
        "params": {
            "sessionId": SESSION_ID,
            "toolCall": { "toolCallId": "tool-2", "title": "Persist trust" },
            "options": [{
                "optionId": "allow-always",
                "name": "Allow always",
                "kind": "allow_always"
            }]
        }
    });
    let observation = codec.decode_frame(callback.to_string().as_bytes()).unwrap();
    let AcpV1Observation::PermissionRequested(request) = observation else {
        panic!("expected permission request");
    };

    assert!(matches!(
        codec.permission_response_frames(
            &request.request_id,
            AcpV1PermissionDecision::Selected {
                option_id: "allow-always".to_owned(),
            },
        ),
        Err(AcpV1CodecError::UnsupportedPermissionOption { .. })
    ));
}

#[test]
fn cancel_settles_every_pending_permission() {
    let mut codec = codec();
    initialize(&mut codec, json!({}));
    open_session(&mut codec);
    start_prompt(&mut codec);
    codec
        .decode_frame(permission_request(json!(41)).to_string().as_bytes())
        .unwrap();
    codec
        .decode_frame(
            permission_request(json!("agent-request-2"))
                .to_string()
                .as_bytes(),
        )
        .unwrap();

    let frames = codec.cancel_frames().unwrap();
    assert_eq!(frames.len(), 3);
    let cancel: Value = serde_json::from_str(&frames[0]).unwrap();
    assert_eq!(cancel["method"], "session/cancel");
    assert_eq!(cancel["params"]["sessionId"], SESSION_ID);
    for frame in &frames[1..] {
        let response: Value = serde_json::from_str(frame).unwrap();
        assert_eq!(response["result"]["outcome"]["outcome"], "cancelled");
    }
    assert!(matches!(
        codec.cancel_frames(),
        Err(AcpV1CodecError::CancellationAlreadySent)
    ));
    assert!(matches!(
        codec.permission_response_frames(
            &AcpV1RequestId::Number(41),
            AcpV1PermissionDecision::Selected {
                option_id: "allow-once".to_owned()
            }
        ),
        Err(AcpV1CodecError::UnknownPermissionRequest(_))
    ));
    let late_update = json!({
        "jsonrpc": "2.0",
        "method": "session/update",
        "params": {
            "sessionId": SESSION_ID,
            "update": {
                "sessionUpdate": "agent_message_chunk",
                "content": { "type": "text", "text": "late" }
            }
        }
    });
    assert!(matches!(
        codec.decode_frame(late_update.to_string().as_bytes()),
        Err(AcpV1CodecError::CancellationAlreadySent)
    ));
}

#[test]
fn unadvertised_callback_gets_correlated_method_not_found() {
    let mut codec = codec();
    initialize(&mut codec, json!({}));
    open_session(&mut codec);
    start_prompt(&mut codec);
    let callback = json!({
        "jsonrpc": "2.0",
        "id": "agent-fs-1",
        "method": "fs/read_text_file",
        "params": { "sessionId": SESSION_ID, "path": "/etc/passwd" }
    });
    let observation = codec.decode_frame(callback.to_string().as_bytes()).unwrap();
    let AcpV1Observation::UnsupportedClientRequest { request_id, method } = observation else {
        panic!("expected unsupported client request");
    };
    assert_eq!(method, "fs/read_text_file");
    let frames = codec
        .reject_unsupported_request_frames(&request_id)
        .unwrap();
    assert_eq!(frames.len(), 1);
    let frame = &frames[0];
    let response: Value = serde_json::from_str(frame).unwrap();
    assert_eq!(response["id"], "agent-fs-1");
    assert_eq!(response["error"]["code"], -32601);
}

#[test]
fn unadvertised_fs_and_terminal_matrix_has_zero_host_io() {
    let workspace = tempfile::tempdir().unwrap();
    let sentinel = workspace.path().join("must-not-exist");
    let mut codec = codec();
    initialize(&mut codec, json!({}));
    open_session(&mut codec);
    start_prompt(&mut codec);

    for (index, method) in [
        "fs/read_text_file",
        "fs/write_text_file",
        "terminal/create",
        "terminal/output",
        "terminal/release",
        "terminal/wait_for_exit",
        "terminal/kill",
    ]
    .into_iter()
    .enumerate()
    {
        let callback = json!({
            "jsonrpc": "2.0",
            "id": index as i64 + 100,
            "method": method,
            "params": {
                "sessionId": SESSION_ID,
                "path": sentinel,
                "command": "/bin/touch",
                "args": [sentinel]
            }
        });
        let AcpV1Observation::UnsupportedClientRequest {
            request_id,
            method: observed,
        } = codec.decode_frame(callback.to_string().as_bytes()).unwrap()
        else {
            panic!("expected unsupported callback")
        };
        assert_eq!(observed, method);
        let response = codec
            .reject_unsupported_request_frames(&request_id)
            .unwrap();
        let response: Value = serde_json::from_str(&response[0]).unwrap();
        assert_eq!(response["error"]["code"], -32601);
        assert!(!sentinel.exists());
    }
}

#[test]
fn top_level_malformed_json_makes_dedicated_stream_terminal() {
    let mut malformed = codec();
    malformed.initialize_frame().unwrap();
    assert!(matches!(
        malformed.decode_transport_frame(b"not-json"),
        Err(AcpV1CodecError::Sdk(_))
    ));
    assert_eq!(malformed.phase(), AcpV1ProtocolPhase::Terminal);
}

#[test]
fn oversized_frame_makes_codec_terminal() {
    let mut oversized = AcpV1Codec::new(AcpV1ClientConfig::new("cosh", "1", 512)).unwrap();
    oversized.initialize_frame().unwrap();
    let frame = vec![b'x'; 513];
    assert!(matches!(
        oversized.decode_frame(&frame),
        Err(AcpV1CodecError::FrameTooLarge { limit: 512 })
    ));
    assert_eq!(oversized.phase(), AcpV1ProtocolPhase::Terminal);
}

#[test]
fn client_config_enforces_frame_safety_ceiling() {
    assert!(matches!(
        AcpV1Codec::new(AcpV1ClientConfig::new("cosh", "1", 0)),
        Err(AcpV1CodecError::InvalidFrameLimit { actual: 0, .. })
    ));
    assert!(matches!(
        AcpV1Codec::new(AcpV1ClientConfig::new("cosh", "1", 1024 * 1024 + 1)),
        Err(AcpV1CodecError::InvalidFrameLimit { .. })
    ));
}

#[test]
fn prompt_cannot_finish_with_pending_permission() {
    let mut codec = codec();
    initialize(&mut codec, json!({}));
    open_session(&mut codec);
    start_prompt(&mut codec);
    codec
        .decode_frame(permission_request(json!(41)).to_string().as_bytes())
        .unwrap();
    let result = json!({
        "jsonrpc": "2.0",
        "id": "cosh-acp-3",
        "result": { "stopReason": "end_turn" }
    });

    assert!(matches!(
        codec.decode_frame(result.to_string().as_bytes()),
        Err(AcpV1CodecError::PromptFinishedWithPendingPermissions { count: 1 })
    ));
    assert_eq!(codec.phase(), AcpV1ProtocolPhase::Terminal);
}

#[test]
fn cancelled_prompt_reports_and_clears_pending_permissions() {
    let mut codec = codec();
    initialize(&mut codec, json!({}));
    open_session(&mut codec);
    start_prompt(&mut codec);
    codec
        .decode_frame(permission_request(json!(41)).to_string().as_bytes())
        .unwrap();
    let result = json!({
        "jsonrpc": "2.0",
        "id": "cosh-acp-3",
        "result": { "stopReason": "cancelled" }
    });

    assert_eq!(
        codec.decode_frame(result.to_string().as_bytes()).unwrap(),
        AcpV1Observation::PromptCancelledWithPendingPermissions {
            session_id: SESSION_ID.to_owned(),
            request_ids: vec![AcpV1RequestId::Number(41)],
        }
    );

    // The cancelled callback cannot receive a late decision or be replayed.
    assert!(matches!(
        codec.permission_response_frames(
            &AcpV1RequestId::Number(41),
            AcpV1PermissionDecision::Cancelled,
        ),
        Err(AcpV1CodecError::UnknownPermissionRequest(_))
    ));
}

#[test]
fn pending_agent_callback_count_is_bounded() {
    let mut codec = codec();
    initialize(&mut codec, json!({}));
    open_session(&mut codec);
    for request_id in 0..64 {
        let callback = json!({
            "jsonrpc": "2.0",
            "id": request_id,
            "method": "fs/read_text_file",
            "params": { "sessionId": SESSION_ID, "path": "/tmp/input" }
        });
        assert!(matches!(
            codec.decode_frame(callback.to_string().as_bytes()),
            Ok(AcpV1Observation::UnsupportedClientRequest { .. })
        ));
    }
    let overflow = json!({
        "jsonrpc": "2.0",
        "id": 64,
        "method": "fs/read_text_file",
        "params": { "sessionId": SESSION_ID, "path": "/tmp/input" }
    });
    assert!(matches!(
        codec.decode_frame(overflow.to_string().as_bytes()),
        Err(AcpV1CodecError::TooManyPendingClientRequests { limit: 64 })
    ));
    assert_eq!(codec.phase(), AcpV1ProtocolPhase::Terminal);
}

#[cfg(unix)]
#[test]
fn bridge_runs_v1_exchange_over_supervised_stdio() {
    let workspace = tempfile::tempdir().unwrap();
    let log_path = workspace.path().join("requests.jsonl");
    let batch_reply_path = workspace.path().join("batch-reply.json");
    let script = r#"
step=0
while IFS= read -r line; do
    step=$((step + 1))
    printf '%s\n' "$line" >> "$1"
    case "$step" in
        1)
            printf '%s\n' '{"jsonrpc":"2.0","id":"cosh-acp-1","result":{"protocolVersion":1,"agentCapabilities":{},"agentInfo":{"name":"stdio-fake","version":"1.0"}}}'
            ;;
        2)
            printf '%s\n' '{"jsonrpc":"2.0","id":"cosh-acp-2","result":{"sessionId":"agent-session-1"}}'
            ;;
        3)
            printf '%s\n' '[17,{"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"agent-session-1","update":{"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":"hello"}}}}]'
            IFS= read -r batch_reply
            printf '%s\n' "$batch_reply" > "$2"
            printf '%s\n' '{"jsonrpc":"2.0","id":"cosh-acp-3","result":{"stopReason":"end_turn"}}'
            ;;
    esac
done
"#;
    let mut spec = RuntimeLaunchSpec::new("/bin/sh", workspace.path());
    spec.arguments = vec![
        "-c".into(),
        script.into(),
        "acp-fake".into(),
        log_path.clone().into_os_string(),
        batch_reply_path.clone().into_os_string(),
    ];
    let mut bridge = AcpV1RuntimeBridge::launch(
        &spec,
        AcpV1ClientConfig::new("cosh-ng", "0.15.0", FRAME_LIMIT),
    )
    .unwrap();

    bridge.send_initialize().unwrap();
    assert!(matches!(
        bridge.read_observation().unwrap(),
        Some(AcpV1Observation::Initialized { .. })
    ));
    assert_eq!(bridge.runtime_state(), RuntimeState::Ready);

    bridge
        .send_new_session(workspace.path(), Vec::new())
        .unwrap();
    assert!(matches!(
        bridge.read_observation().unwrap(),
        Some(AcpV1Observation::SessionOpened { .. })
    ));
    bridge.send_prompt("hello over ACP").unwrap();
    assert!(matches!(
        bridge.read_observation().unwrap(),
        Some(AcpV1Observation::SessionUpdate { .. })
    ));
    assert_eq!(
        bridge.read_observation().unwrap(),
        Some(AcpV1Observation::PromptFinished {
            session_id: SESSION_ID.to_owned(),
            stop_reason: AcpV1StopReason::EndTurn,
        })
    );
    bridge.shutdown(Duration::from_secs(1)).unwrap();

    let requests = std::fs::read_to_string(log_path).unwrap();
    let requests = requests
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).unwrap())
        .collect::<Vec<_>>();
    assert_eq!(requests.len(), 3);
    assert_eq!(requests[0]["method"], "initialize");
    assert_eq!(requests[0]["params"]["protocolVersion"], 1);
    assert_eq!(requests[1]["method"], "session/new");
    assert_eq!(requests[2]["method"], "session/prompt");
    let batch_reply: Value =
        serde_json::from_str(&std::fs::read_to_string(batch_reply_path).unwrap()).unwrap();
    let batch_reply = batch_reply.as_array().unwrap();
    assert_eq!(batch_reply.len(), 1);
    assert_eq!(batch_reply[0]["id"], Value::Null);
    assert_eq!(batch_reply[0]["error"]["code"], -32600);
}

#[cfg(unix)]
#[test]
fn bridge_reports_codex_typed_terminal_failure_after_partial_text() {
    let workspace = tempfile::tempdir().unwrap();
    let script = r#"
step=0
while IFS= read -r line; do
    step=$((step + 1))
    case "$step" in
        1)
            printf '%s\n' '{"jsonrpc":"2.0","id":"cosh-acp-1","result":{"protocolVersion":1,"agentCapabilities":{},"agentInfo":{"name":"@agentclientprotocol/codex-acp","title":"Codex","version":"1.6.2"},"_meta":{"jetbrains":{"air":{"version":1,"capabilities":["sessionFailure","agentFileChangeReport"]}}}}}'
            ;;
        2)
            printf '%s\n' '{"jsonrpc":"2.0","id":"cosh-acp-2","result":{"sessionId":"agent-session-1"}}'
            ;;
        3)
            printf '%s\n' '{"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"agent-session-1","update":{"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":"partial"}}}}'
            printf '%s\n' '{"jsonrpc":"2.0","id":"cosh-acp-3","result":{"stopReason":"end_turn","_meta":{"jetbrains":{"air":{"version":1,"sessionFailure":{"id":"turn-1:error","revision":1,"category":"service","severity":"error","title":"provider failed","actions":["retry"]}}}}}}'
            ;;
    esac
done
"#;
    let mut spec = RuntimeLaunchSpec::new("/bin/sh", workspace.path());
    spec.arguments = vec!["-c".into(), script.into()];
    let mut bridge = AcpV1RuntimeBridge::launch(
        &spec,
        AcpV1ClientConfig::new("cosh-ng", "0.15.0", FRAME_LIMIT)
            .adapter_profile(AcpV1AdapterProfile::Codex162),
    )
    .unwrap();

    bridge.send_initialize().unwrap();
    assert!(matches!(
        bridge.read_observation().unwrap(),
        Some(AcpV1Observation::Initialized { .. })
    ));
    bridge
        .send_new_session(workspace.path(), Vec::new())
        .unwrap();
    assert!(matches!(
        bridge.read_observation().unwrap(),
        Some(AcpV1Observation::SessionOpened { .. })
    ));
    bridge.send_prompt("test typed failure").unwrap();
    assert!(matches!(
        bridge.read_observation().unwrap(),
        Some(AcpV1Observation::SessionUpdate { .. })
    ));
    assert!(matches!(
        bridge.read_observation().unwrap(),
        Some(AcpV1Observation::RequestFailed {
            request: AcpV1RequestKind::Prompt,
            code: -32000,
            ..
        })
    ));
    bridge.shutdown(Duration::from_secs(1)).unwrap();
}

#[cfg(unix)]
#[test]
fn bridge_reaps_agent_that_closes_stdout_without_exiting() {
    let workspace = tempfile::tempdir().unwrap();
    let script = "exec 1>&-; sleep 60";
    let mut spec = RuntimeLaunchSpec::new("/bin/sh", workspace.path());
    spec.arguments = vec!["-c".into(), script.into()];
    let mut bridge = AcpV1RuntimeBridge::launch(
        &spec,
        AcpV1ClientConfig::new("cosh-ng", "0.15.0", FRAME_LIMIT),
    )
    .unwrap();

    bridge.send_initialize().unwrap();
    assert_eq!(
        bridge.read_observation().unwrap(),
        Some(AcpV1Observation::TransportClosed)
    );
    assert_eq!(bridge.protocol_phase(), AcpV1ProtocolPhase::Terminal);
    assert_eq!(bridge.runtime_state(), RuntimeState::Exited);
}

#[cfg(unix)]
#[test]
fn bridge_fail_closes_adversarial_stdout_and_reaps_once() {
    for output in [
        "printf '%s\\n' 'stdout contamination'",
        "printf '\\377\\n'",
        "head -c 257 /dev/zero | tr '\\000' x; printf '\\n'",
    ] {
        let workspace = tempfile::tempdir().unwrap();
        let script = format!("read -r initialize; {output}; while :; do sleep 1; done");
        let mut spec = RuntimeLaunchSpec::new("/bin/sh", workspace.path());
        spec.arguments = vec!["-c".into(), script.into()];
        spec.stdout_line_limit = 256;
        let mut bridge = AcpV1RuntimeBridge::launch(
            &spec,
            AcpV1ClientConfig::new("cosh-ng", "0.15.0", FRAME_LIMIT),
        )
        .unwrap();

        bridge.send_initialize().unwrap();
        assert!(bridge.read_observation().is_err());
        assert_eq!(bridge.protocol_phase(), AcpV1ProtocolPhase::Terminal);
        assert_eq!(bridge.runtime_state(), RuntimeState::Exited);
        assert!(bridge.poll_terminal().unwrap().is_some());
        assert!(bridge.poll_terminal().unwrap().is_none());
    }
}
