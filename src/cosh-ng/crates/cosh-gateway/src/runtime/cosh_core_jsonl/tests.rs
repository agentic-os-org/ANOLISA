//! Focused private cosh-core JSONL codec tests.

use super::*;
use serde_json::Value;

fn private_wire_corpus() -> Value {
    serde_json::from_str(include_str!(
        "../../../../cosh-core/tests/fixtures/cosh-private-wire-dual-version.json"
    ))
    .expect("valid private wire corpus")
}

fn fixture_frame(value: &Value) -> Vec<u8> {
    serde_json::to_vec(value).expect("serializable private wire fixture")
}

fn initialized_codec() -> CoshCoreJsonlCodec {
    let corpus = private_wire_corpus();
    let mut codec = CoshCoreJsonlCodec::new("gateway-init-1", 4096).unwrap();
    codec.initialize_frame(false).unwrap();
    let response = fixture_frame(&corpus["legacy_v1"]["initialize_ack"]);
    assert!(matches!(
        codec.decode_frame(&response).unwrap(),
        CoshCoreObservation::Initialized(_)
    ));
    codec
}

fn initialized_brokered_codec() -> CoshCoreJsonlCodec {
    let corpus = private_wire_corpus();
    let mut codec = CoshCoreJsonlCodec::new_gateway_brokered("gateway-init-v3", 4096).unwrap();
    codec.initialize_frame(false).unwrap();
    let response = fixture_frame(&corpus["gateway_brokered_v3"]["initialize_ack"]);
    assert!(matches!(
        codec.decode_frame(&response).unwrap(),
        CoshCoreObservation::Initialized(_)
    ));
    codec
}

fn initialized_checkpoint_codec() -> CoshCoreJsonlCodec {
    let corpus = private_wire_corpus();
    let mut codec =
        CoshCoreJsonlCodec::new_gateway_brokered_checkpoint("gateway-init-v4", 4096).unwrap();
    codec.initialize_frame(false).unwrap();
    let response = fixture_frame(&corpus["gateway_brokered_checkpoint_v4"]["initialize_ack"]);
    assert!(matches!(
        codec.decode_frame(&response).unwrap(),
        CoshCoreObservation::Initialized(_)
    ));
    codec
}

fn workspace_write_acknowledgement(request_id: &str) -> Value {
    serde_json::json!({
        "type": "control_response",
        "response": {
            "subtype": "success",
            "request_id": request_id,
            "response": {
                "subtype": "initialize",
                "protocol_version": 5,
                "execution_profile": "gateway_brokered_workspace_write_v1",
                "capability_profile": {
                    "profile_id": "workspace-write-v1",
                    "manifest_digest": "30574302eeba3adbb5ea143a8a869331d58a15bd24b9532d0f52613136bb2b2a"
                },
                "runtime_tools": ["ask_user_question", "write_file"],
                "capabilities": {
                    "can_handle_can_use_tool": true,
                    "can_handle_host_executed_shell_tool_result": false,
                    "can_handle_shell_evidence_tool": false,
                    "can_handle_approval_receipt": true,
                    "can_handle_brokered_ask_user": true
                }
            }
        }
    })
}

fn initialized_workspace_write_codec() -> CoshCoreJsonlCodec {
    let mut codec =
        CoshCoreJsonlCodec::new_gateway_brokered_workspace_write("gateway-init-v5", 4096).unwrap();
    codec.initialize_frame(false).unwrap();
    assert!(matches!(
        codec
            .decode_frame(&fixture_frame(&workspace_write_acknowledgement(
                "gateway-init-v5"
            )))
            .unwrap(),
        CoshCoreObservation::Initialized(_)
    ));
    codec
}

#[test]
fn initialize_is_explicitly_private_version_one() {
    let corpus = private_wire_corpus();
    let mut codec = CoshCoreJsonlCodec::new("gateway-init-1", 4096).unwrap();

    let frame = codec.initialize_frame(false).unwrap();
    let value: Value = serde_json::from_str(frame.trim()).unwrap();

    assert_eq!(value, corpus["legacy_v1"]["initialize_request"]);
    assert_eq!(codec.phase(), CoshCoreProtocolPhase::AwaitingInitialize);
}

#[test]
fn brokered_initialize_is_exact_private_v3_and_profile_bound() {
    let corpus = private_wire_corpus();
    let mut codec = CoshCoreJsonlCodec::new_gateway_brokered("gateway-init-v3", 4096).unwrap();
    let frame = codec.initialize_frame(false).unwrap();
    let value: Value = serde_json::from_str(frame.trim()).unwrap();
    assert_eq!(value, corpus["gateway_brokered_v3"]["initialize_request"]);
}

#[test]
fn checkpoint_initialize_is_exact_private_v4_and_profile_bound() {
    let corpus = private_wire_corpus();
    let mut codec =
        CoshCoreJsonlCodec::new_gateway_brokered_checkpoint("gateway-init-v4", 4096).unwrap();
    let frame = codec.initialize_frame(false).unwrap();
    let value: Value = serde_json::from_str(frame.trim()).unwrap();

    assert_eq!(
        value,
        corpus["gateway_brokered_checkpoint_v4"]["initialize_request"]
    );
}

#[test]
fn workspace_write_initialize_is_exact_private_v5_and_profile_bound() {
    let mut codec =
        CoshCoreJsonlCodec::new_gateway_brokered_workspace_write("gateway-init-v5", 4096).unwrap();
    let frame = codec.initialize_frame(false).unwrap();
    let value: Value = serde_json::from_str(frame.trim()).unwrap();
    assert_eq!(
        value,
        serde_json::json!({
            "type": "control_request",
            "request_id": "gateway-init-v5",
            "request": {
                "subtype": "initialize",
                "fire_session_start": false,
                "protocol_version": 5,
                "execution_profile": "gateway_brokered_workspace_write_v1",
                "capability_profile": {
                    "profile_id": "workspace-write-v1",
                    "manifest_digest": "30574302eeba3adbb5ea143a8a869331d58a15bd24b9532d0f52613136bb2b2a"
                }
            }
        })
    );
}

#[test]
fn workspace_write_initialize_rejects_inventory_or_profile_drift() {
    for runtime_tools in [
        serde_json::json!(["write_file", "ask_user_question"]),
        serde_json::json!(["ask_user_question"]),
    ] {
        let mut acknowledgement = workspace_write_acknowledgement("gateway-init-v5");
        acknowledgement["response"]["response"]["runtime_tools"] = runtime_tools;
        let mut codec =
            CoshCoreJsonlCodec::new_gateway_brokered_workspace_write("gateway-init-v5", 4096)
                .unwrap();
        codec.initialize_frame(false).unwrap();
        assert!(matches!(
            codec.decode_frame(&fixture_frame(&acknowledgement)),
            Err(CoshCoreCodecError::InitializeCapabilitiesInvalid)
        ));
    }

    let mut acknowledgement = workspace_write_acknowledgement("gateway-init-v5");
    acknowledgement["response"]["response"]["capability_profile"]["profile_id"] =
        serde_json::json!("task-only-v1");
    let mut codec =
        CoshCoreJsonlCodec::new_gateway_brokered_workspace_write("gateway-init-v5", 4096).unwrap();
    codec.initialize_frame(false).unwrap();
    assert!(matches!(
        codec.decode_frame(&fixture_frame(&acknowledgement)),
        Err(CoshCoreCodecError::InitializeProfileMismatch)
    ));
}

#[test]
fn workspace_write_permission_frames_are_closed_golden_shapes() {
    let codec = initialized_workspace_write_codec();
    assert_eq!(
        codec.core_permission_allow_frame("write-1").unwrap(),
        "{\"type\":\"control_response\",\"response\":{\"subtype\":\"success\",\"request_id\":\"write-1\",\"response\":{\"behavior\":\"allow\"}}}\n"
    );
    assert_eq!(
        codec.brokered_denial_frame("write-1", "Denied safely").unwrap(),
        "{\"type\":\"control_response\",\"response\":{\"subtype\":\"success\",\"request_id\":\"write-1\",\"response\":{\"behavior\":\"deny\",\"message\":\"Denied safely\"}}}\n"
    );
}

#[test]
fn checkpoint_initialize_rejects_capability_or_inventory_drift() {
    let acknowledged = |runtime_tools: Value, hosted_checkpoint: bool| {
        serde_json::json!({
            "type": "control_response",
            "response": {
                "subtype": "success",
                "request_id": "gateway-init-v4",
                "response": {
                    "subtype": "initialize",
                    "protocol_version": 4,
                    "execution_profile": "gateway_brokered_checkpoint_v1",
                    "capability_profile": {
                        "profile_id": "workspace-checkpoint-v1",
                        "manifest_digest": "6b3e7093e7b8656d4a7cf21faa85b9eed761ef415d002623cfc442f3ef3c8ae1"
                    },
                    "runtime_tools": runtime_tools,
                    "capabilities": {
                        "can_handle_can_use_tool": true,
                        "can_handle_host_executed_shell_tool_result": false,
                        "can_handle_approval_receipt": true,
                        "can_handle_hosted_checkpoint_create": hosted_checkpoint,
                        "can_handle_brokered_ask_user": true
                    }
                }
            }
        })
    };
    for acknowledgement in [
        acknowledged(
            serde_json::json!(["workspace_checkpoint_create", "ask_user_question"]),
            true,
        ),
        acknowledged(serde_json::json!(["ask_user_question"]), true),
        acknowledged(
            serde_json::json!(["ask_user_question", "workspace_checkpoint_create"]),
            false,
        ),
    ] {
        let mut codec =
            CoshCoreJsonlCodec::new_gateway_brokered_checkpoint("gateway-init-v4", 4096).unwrap();
        codec.initialize_frame(false).unwrap();
        assert!(matches!(
            codec.decode_frame(&fixture_frame(&acknowledgement)),
            Err(CoshCoreCodecError::InitializeCapabilitiesInvalid)
        ));
    }
}

#[test]
fn brokered_initialize_rejects_missing_or_wrong_profile_and_capabilities() {
    let corpus = private_wire_corpus();
    let invalid = corpus["gateway_brokered_v3"]["invalid_initialize_acks"]
        .as_object()
        .expect("invalid initialize acknowledgement map");

    for case in [
        "missing_profile",
        "wrong_profile",
        "missing_capability_profile",
        "drifted_capability_profile",
    ] {
        let mut codec = CoshCoreJsonlCodec::new_gateway_brokered("gateway-init-v3", 4096).unwrap();
        codec.initialize_frame(false).unwrap();
        let response = fixture_frame(&invalid[case]);
        assert!(
            matches!(
                codec.decode_frame(&response),
                Err(CoshCoreCodecError::InitializeProfileMismatch)
            ),
            "case {case}"
        );
    }

    for (case, expected) in [
        ("missing_version", "missing_version"),
        ("wrong_version", "wrong_version"),
        ("missing_capabilities", "missing_capabilities"),
        ("wrong_capabilities", "wrong_capabilities"),
    ] {
        let mut codec = CoshCoreJsonlCodec::new_gateway_brokered("gateway-init-v3", 4096).unwrap();
        codec.initialize_frame(false).unwrap();
        let response = fixture_frame(&invalid[case]);
        let error = codec.decode_frame(&response).unwrap_err();
        assert!(
            match expected {
                "missing_version" => matches!(error, CoshCoreCodecError::InitializeVersionMissing),
                "wrong_version" => matches!(
                    error,
                    CoshCoreCodecError::InitializeVersionMismatch {
                        required: 3,
                        actual: 2
                    }
                ),
                "missing_capabilities" => {
                    matches!(error, CoshCoreCodecError::InitializeCapabilitiesMissing)
                }
                "wrong_capabilities" => {
                    matches!(error, CoshCoreCodecError::InitializeCapabilitiesInvalid)
                }
                _ => false,
            },
            "case {case}: {error:?}"
        );
    }
}

#[test]
fn brokered_initialize_rejects_runtime_tool_inventory_drift() {
    let corpus = private_wire_corpus();
    let mut missing = corpus["gateway_brokered_v3"]["initialize_ack"].clone();
    missing["response"]["response"]
        .as_object_mut()
        .unwrap()
        .remove("runtime_tools");
    let mut null = corpus["gateway_brokered_v3"]["initialize_ack"].clone();
    null["response"]["response"]["runtime_tools"] = serde_json::json!(null);
    for acknowledgement in [missing, null] {
        let mut codec = CoshCoreJsonlCodec::new_gateway_brokered("gateway-init-v3", 4096).unwrap();
        codec.initialize_frame(false).unwrap();
        assert!(matches!(
            codec.decode_frame(&fixture_frame(&acknowledgement)),
            Err(CoshCoreCodecError::InitializeCapabilitiesMissing)
        ));
    }
    for runtime_tools in [
        serde_json::json!([]),
        serde_json::json!(["ask_user_question", "shell"]),
    ] {
        let mut acknowledgement = corpus["gateway_brokered_v3"]["initialize_ack"].clone();
        acknowledgement["response"]["response"]["runtime_tools"] = runtime_tools;
        let mut codec = CoshCoreJsonlCodec::new_gateway_brokered("gateway-init-v3", 4096).unwrap();
        codec.initialize_frame(false).unwrap();
        assert!(matches!(
            codec.decode_frame(&fixture_frame(&acknowledgement)),
            Err(CoshCoreCodecError::InitializeCapabilitiesInvalid)
        ));
    }
}

#[test]
fn brokered_callback_frames_are_closed_golden_shapes() {
    let corpus = private_wire_corpus();
    let codec = initialized_brokered_codec();
    let acknowledgement: Value = serde_json::from_str(
        codec
            .brokered_acknowledgement_frame("req-1")
            .unwrap()
            .trim(),
    )
    .unwrap();
    assert_eq!(
        acknowledgement,
        serde_json::json!({"type": "approval_receipt", "request_id": "req-1"})
    );
    assert_eq!(
        codec.brokered_denial_frame("req-1", "denied").unwrap(),
        "{\"type\":\"control_response\",\"response\":{\"subtype\":\"success\",\"request_id\":\"req-1\",\"response\":{\"behavior\":\"deny\",\"message\":\"denied\"}}}\n"
    );
    let answer: Value = serde_json::from_str(
        codec
            .brokered_input_response_frame("question-1", "main")
            .unwrap()
            .trim(),
    )
    .unwrap();
    assert_eq!(answer, corpus["gateway_brokered_v3"]["ask_user_answer"]);
}

#[test]
fn checkpoint_terminal_frames_are_typed_and_hide_gateway_authority() {
    use cosh_gateway_contracts::{
        common::{BoundedOpaque, BoundedText},
        ids::{CheckpointId, ExecutionId, RequestId},
        runtime::{
            BrokeredExecutionDelivery, BrokeredExecutionOutcome, BrokeredOperationResult,
            WorkspaceCheckpointCreateV1Outcome, WorkspaceCheckpointCreateV1Result,
        },
    };

    let corpus = private_wire_corpus();
    let codec = initialized_checkpoint_codec();
    let request_id = RequestId::new();
    let checkpoint_id = CheckpointId::parse("ckp_123e4567-e89b-12d3-a456-426614174000").unwrap();
    let success = BrokeredExecutionDelivery {
        request_id: request_id.clone(),
        outcome: BrokeredExecutionOutcome::Succeeded {
            execution_id: ExecutionId::new(),
            result: BrokeredOperationResult::WorkspaceCheckpointCreateV1(
                WorkspaceCheckpointCreateV1Result {
                    checkpoint_id: checkpoint_id.clone(),
                    outcome: WorkspaceCheckpointCreateV1Outcome::Created {
                        snapshot_id: BoundedOpaque::new("snap-1").unwrap(),
                    },
                },
            ),
        },
    };
    let success: Value = serde_json::from_str(
        codec
            .brokered_checkpoint_result_frame("checkpoint-request-1", &success)
            .unwrap()
            .trim(),
    )
    .unwrap();
    assert_eq!(
        success,
        corpus["gateway_brokered_checkpoint_v4"]["checkpoint_created"]
    );
    assert!(success.to_string().find("execution_id").is_none());
    assert!(success.to_string().find(request_id.as_str()).is_none());

    let denied = BrokeredExecutionDelivery {
        request_id: RequestId::new(),
        outcome: BrokeredExecutionOutcome::Denied {
            code: cosh_gateway_contracts::capability::DenialCode::ApprovalDenied,
            safe_message: BoundedText::new("denied once").unwrap(),
        },
    };
    let denied = codec
        .brokered_checkpoint_result_frame("private-checkpoint-2", &denied)
        .unwrap();
    assert!(denied.contains(r#""behavior":"deny","message":"denied once""#));

    for (outcome, expected) in [
        (
            BrokeredExecutionOutcome::Failed {
                execution_id: ExecutionId::new(),
                error: cosh_gateway_contracts::error::ContractError::new(
                    "checkpoint_failed",
                    cosh_gateway_contracts::error::ErrorCategory::Internal,
                    false,
                    "Checkpoint creation failed",
                )
                .unwrap(),
            },
            serde_json::json!({
                "behavior": "host_executed_checkpoint_error",
                "checkpointError": {
                    "outcome": "failed",
                    "code": "checkpoint_failed",
                    "message": "Checkpoint creation failed"
                }
            }),
        ),
        (
            BrokeredExecutionOutcome::Uncertain {
                execution_id: ExecutionId::new(),
                error: cosh_gateway_contracts::error::ContractError::new(
                    "checkpoint_unknown",
                    cosh_gateway_contracts::error::ErrorCategory::Transport,
                    false,
                    "Exact evidence was not available",
                )
                .unwrap(),
            },
            corpus["gateway_brokered_checkpoint_v4"]["checkpoint_uncertain"]["response"]
                ["response"]
                .clone(),
        ),
    ] {
        let delivery = BrokeredExecutionDelivery {
            request_id: RequestId::new(),
            outcome,
        };
        let frame: Value = serde_json::from_str(
            codec
                .brokered_checkpoint_result_frame("private-checkpoint-error", &delivery)
                .unwrap()
                .trim(),
        )
        .unwrap();
        assert_eq!(frame["response"]["response"], expected);
        assert!(frame.to_string().find("execution_id").is_none());
    }
}

#[test]
fn brokered_ask_user_request_is_strictly_typed() {
    let corpus = private_wire_corpus();
    let mut codec = initialized_brokered_codec();
    let request = fixture_frame(&corpus["gateway_brokered_v3"]["ask_user_request"]);

    assert_eq!(
        codec.decode_frame(&request).unwrap(),
        CoshCoreObservation::ControlRequest(CoshCoreControlRequestEnvelope {
            request_id: "question-1".to_string(),
            request: CoshCoreControlRequest::AskUser {
                tool_use_id: Some("question-call".to_string()),
                question: "Choose a branch".to_string(),
                options: vec![CoshCoreAskUserOption {
                    label: "main".to_string(),
                    description: Some("Use the default branch".to_string()),
                }],
                allow_free_text: true,
                multi_select: false,
            },
        })
    );

    let malformed = br#"{"type":"control_request","request_id":"question-2","request":{"subtype":"ask_user","tool_use_id":"question-call","question":"q","options":[{"label":"x","secret":"raw"}],"allow_free_text":true,"multi_select":false}}"#;
    assert!(matches!(
        codec.decode_frame(malformed),
        Err(CoshCoreCodecError::Malformed(_))
    ));
}

#[test]
fn checkpoint_request_is_strictly_typed_before_bridge_normalization() {
    let mut codec = initialized_checkpoint_codec();
    let request = br#"{"type":"control_request","request_id":"checkpoint-1","request":{"subtype":"can_use_tool","tool_name":"workspace_checkpoint_create","input":{},"tool_use_id":"checkpoint-call","hook_requires_approval":true}}"#;
    assert!(matches!(
        codec.decode_frame(request).unwrap(),
        CoshCoreObservation::ControlRequest(CoshCoreControlRequestEnvelope {
            request: CoshCoreControlRequest::CanUseTool { ref tool_name, ref input, .. },
            ..
        }) if tool_name == "workspace_checkpoint_create" && input == &serde_json::json!({})
    ));

    let unknown = br#"{"type":"control_request","request_id":"checkpoint-2","request":{"subtype":"can_use_tool","tool_name":"workspace_checkpoint_create","input":{},"tool_use_id":"checkpoint-call","hidden_authority":"forbidden"}}"#;
    assert!(matches!(
        codec.decode_frame(unknown),
        Err(CoshCoreCodecError::Malformed(_))
    ));
}

#[test]
fn legacy_codec_cannot_encode_brokered_authority() {
    let codec = initialized_codec();
    assert!(matches!(
        codec.brokered_acknowledgement_frame("req-1"),
        Err(CoshCoreCodecError::ProfileMismatch { .. })
    ));
}

#[test]
fn initialization_requires_exact_version_and_correlation() {
    let mut codec = CoshCoreJsonlCodec::new("expected", 4096).unwrap();
    codec.initialize_frame(true).unwrap();
    let mismatched = br#"{"type":"control_response","response":{"subtype":"success","request_id":"other","response":{"subtype":"initialize","protocol_version":1,"capabilities":{}}}}"#;

    assert!(matches!(
        codec.decode_frame(mismatched),
        Err(CoshCoreCodecError::InitializeCorrelationMismatch)
    ));

    let wrong_version = br#"{"type":"control_response","response":{"subtype":"success","request_id":"expected","response":{"subtype":"initialize","protocol_version":2,"capabilities":{}}}}"#;
    assert!(matches!(
        codec.decode_frame(wrong_version),
        Err(CoshCoreCodecError::InitializeVersionMismatch {
            required: 1,
            actual: 2
        })
    ));
}

#[test]
fn auth_bootstrap_is_only_control_request_allowed_before_ready() {
    let mut codec = CoshCoreJsonlCodec::new("init", 4096).unwrap();
    codec.initialize_frame(true).unwrap();
    let auth = br#"{"type":"control_request","request_id":"auth-1","request":{"subtype":"auth_required","reason":"not_configured","providers":[]}}"#;

    assert!(matches!(
        codec.decode_frame(auth).unwrap(),
        CoshCoreObservation::ControlRequest(CoshCoreControlRequestEnvelope {
            request: CoshCoreControlRequest::AuthRequired { .. },
            ..
        })
    ));
    assert_eq!(codec.phase(), CoshCoreProtocolPhase::AwaitingInitialize);
}

#[test]
fn result_and_eof_produce_one_terminal_observation() {
    let mut codec = initialized_codec();
    let result = br#"{"type":"result","subtype":"success","is_error":false,"result":"done","session_id":"provider-session"}"#;

    assert!(matches!(
        codec.decode_frame(result).unwrap(),
        CoshCoreObservation::Result(CoshCoreResult {
            is_error: false,
            ..
        })
    ));
    assert_eq!(codec.phase(), CoshCoreProtocolPhase::Terminal);
    assert_eq!(codec.finish_stdout(), None);
    assert!(matches!(
        codec.decode_frame(result),
        Err(CoshCoreCodecError::OutputAfterTerminal)
    ));
}

#[test]
fn eof_before_result_is_synthetic_terminal_once() {
    let mut codec = initialized_codec();

    assert_eq!(
        codec.finish_stdout(),
        Some(CoshCoreObservation::ProtocolEndedWithoutResult)
    );
    assert_eq!(codec.finish_stdout(), None);
}

#[test]
fn user_mapping_uses_provider_session_without_gateway_identity() {
    let codec = initialized_codec();
    let frame = codec
        .user_frame(&CoshCoreUserTurn {
            content: "diagnose".to_string(),
            provider_session_id: Some("provider-session".to_string()),
            raw_user_input: Some("diagnose".to_string()),
            shell_context: None,
        })
        .unwrap();
    let value: Value = serde_json::from_str(frame.trim()).unwrap();

    assert_eq!(value["type"], "user");
    assert_eq!(value["session_id"], "provider-session");
    assert_eq!(value["message"]["role"], "user");
}

#[test]
fn duplicate_initialize_response_is_rejected_after_readiness() {
    let mut codec = initialized_codec();
    let duplicate = br#"{"type":"control_response","response":{"subtype":"success","request_id":"gateway-init-1","response":{"subtype":"initialize","protocol_version":1,"capabilities":{}}}}"#;

    assert!(matches!(
        codec.decode_frame(duplicate),
        Err(CoshCoreCodecError::DuplicateInitializeResponse)
    ));
}

#[test]
fn user_output_accepts_only_typed_tool_results() {
    let mut codec = initialized_codec();
    let invalid = br#"{"type":"user","session_id":"provider-session","message":{"content":[{"type":"text","tool_use_id":"tool-1","is_error":false,"content":"not a tool result"}]}}"#;

    assert!(matches!(
        codec.decode_frame(invalid),
        Err(CoshCoreCodecError::Malformed(_))
    ));
}

#[test]
fn user_output_maps_typed_tool_result() {
    let mut codec = initialized_codec();
    let output = br#"{"type":"user","session_id":"provider-session","message":{"content":[{"type":"tool_result","tool_use_id":"tool-1","is_error":false,"content":"done"}]}}"#;

    let observation = codec.decode_frame(output).unwrap();
    assert_eq!(
        observation,
        CoshCoreObservation::ToolResults {
            provider_session_id: "provider-session".to_string(),
            results: vec![CoshCoreToolResult {
                tool_use_id: "tool-1".to_string(),
                is_error: false,
                content: "done".to_string(),
            }],
        }
    );
}

#[test]
fn oversized_output_is_rejected_before_json_allocation() {
    let mut codec = CoshCoreJsonlCodec::new("init", 8).unwrap();
    codec.initialize_frame(true).unwrap_err();
    assert_eq!(codec.phase(), CoshCoreProtocolPhase::Created);
    assert!(matches!(
        codec.decode_frame(b"123456789"),
        Err(CoshCoreCodecError::FrameTooLarge { limit: 8 })
    ));
}
