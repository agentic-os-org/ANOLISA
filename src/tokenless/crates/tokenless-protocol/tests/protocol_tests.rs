use serde_json::json;
use tokenless_protocol::*;

fn attribution() -> Attribution {
    Attribution {
        agent_id: "codex".into(),
        session_id: Some("session-1".into()),
        tool_use_id: Some("call-1".into()),
    }
}

fn requests() -> Vec<RequestEnvelope> {
    vec![
        RequestEnvelope {
            attribution: attribution(),
            request: Request::BeforeModel(BeforeModelRequest {
                tools: vec![json!({"name": "read"})],
                visible_context: json!({"messages": []}),
                capabilities: BeforeModelCapabilities {
                    replace_tools: true,
                    recovery: tokenless_protocol::RecoveryMethod::Shell,
                },
            }),
        },
        RequestEnvelope {
            attribution: attribution(),
            request: Request::PreTool(PreToolRequest {
                tool_name: "Bash".into(),
                arguments: json!({"command": "git status"}),
                command_field: "command".into(),
                capabilities: PreToolCapabilities {
                    replace_arguments: true,
                    block_and_suggest: false,
                },
            }),
        },
        RequestEnvelope {
            attribution: attribution(),
            request: Request::PostTool(PostToolRequest {
                result_kind: ResultKind::Tool,
                tool_name: "Bash".into(),
                content: "{}".into(),
                status: ToolResultStatus::Success,
                content_origin: ContentOrigin::CommandOutput,
                output_optimization: OutputOptimization::None,
                capabilities: PostToolCapabilities {
                    replace_output: true,
                    recovery: tokenless_protocol::RecoveryMethod::None,
                    replace_with_text: true,
                },
            }),
        },
        RequestEnvelope {
            attribution: attribution(),
            request: Request::Retrieve(RetrieveRequest {
                hash_or_marker: "0123456789abcdef01234567".into(),
                visible_markers: vec!["0123456789abcdef01234567".into()],
            }),
        },
    ]
}

#[test]
fn all_request_operations_round_trip_with_fixed_envelope() {
    for request in requests() {
        let json = request.to_json().unwrap();
        let value: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(value["protocol_version"], 2);
        assert_eq!(value["operation"], request.request.operation().wire_str());
        assert!(value.get("input").is_some());
        assert!(value.get("result").is_none());
        assert_eq!(RequestEnvelope::from_json(&json).unwrap(), request);
    }
}

#[test]
fn all_response_operations_round_trip_with_fixed_envelope() {
    let responses = [
        Response::BeforeModel(BeforeModelResponse {
            tools: vec![],
            visible_markers: vec![],
        }),
        Response::PreTool(PreToolResponse {
            arguments: json!({}),
            action: PreToolAction::Passthrough,
            output_optimization: OutputOptimization::None,
        }),
        Response::PostTool(PostToolResponse {
            output: "{}".into(),
            disposition: Disposition::Applied,
            content_type: Some(ContentType::Json),
            applied_operations: vec![
                AppliedOperation::TerminalCleanup,
                AppliedOperation::BuildLogReduction,
                AppliedOperation::JsonCleanup,
                AppliedOperation::JsonRecordReduction,
                AppliedOperation::JsonTruncation,
                AppliedOperation::Toon,
            ],
            recoverability: Recoverability::Lossless,
            before_tokens: 10,
            after_tokens: 4,
            stash_keys: vec![],
            tokenizer_id: TOKENIZER_ID.into(),
            additional_context: None,
        }),
        Response::Retrieve(RetrieveResponse {
            hash: "0123456789abcdef01234567".into(),
            payload: "payload".into(),
        }),
    ];
    for response in responses {
        let envelope = ResponseEnvelope {
            attribution: attribution(),
            response,
        };
        let json = envelope.to_json().unwrap();
        let value: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert!(value.get("result").is_some());
        assert!(value.get("input").is_none());
        assert_eq!(ResponseEnvelope::from_json(&json).unwrap(), envelope);
    }
}

#[test]
fn record_reduction_has_a_stable_wire_name() {
    assert_eq!(
        serde_json::to_string(&AppliedOperation::JsonRecordReduction).unwrap(),
        r#""json_record_reduction""#
    );
    assert_eq!(
        serde_json::to_string(&AppliedOperation::TerminalCleanup).unwrap(),
        r#""terminal_cleanup""#
    );
    assert_eq!(
        serde_json::to_string(&AppliedOperation::BuildLogReduction).unwrap(),
        r#""build_log_reduction""#
    );
    assert_eq!(
        AppliedOperation::JsonRecordReduction.wire_str(),
        "json_record_reduction"
    );
}

#[test]
fn content_operations_have_stable_wire_names() {
    for (operation, name) in [
        (AppliedOperation::TabularCompaction, "tabular_compaction"),
        (AppliedOperation::SearchPathSharing, "search_path_sharing"),
        (AppliedOperation::DiffReduction, "diff_reduction"),
        (
            AppliedOperation::TabularRowReduction,
            "tabular_row_reduction",
        ),
    ] {
        assert_eq!(operation.wire_str(), name);
        let serialized = serde_json::to_string(&operation).unwrap();
        assert_eq!(serialized, format!("\"{name}\""));
        assert_eq!(
            serde_json::from_str::<AppliedOperation>(&serialized).unwrap(),
            operation
        );
    }
}

#[test]
fn operation_payloads_are_isolated_and_strict() {
    let mut value: serde_json::Value =
        serde_json::from_str(&requests()[0].to_json().unwrap()).unwrap();
    value["input"]["command_field"] = json!("command");
    assert!(RequestEnvelope::from_json(&value.to_string()).is_err());

    let mut value: serde_json::Value =
        serde_json::from_str(&requests()[0].to_json().unwrap()).unwrap();
    value["input"]["retrieve_tool_name"] = json!("tokenless_retrieve");
    assert!(RequestEnvelope::from_json(&value.to_string()).is_err());

    let mut value: serde_json::Value =
        serde_json::from_str(&requests()[0].to_json().unwrap()).unwrap();
    value["input"]["capabilities"]["publish_retrieve_tool"] = json!(true);
    assert!(RequestEnvelope::from_json(&value.to_string()).is_err());

    let mut value: serde_json::Value =
        serde_json::from_str(&requests()[2].to_json().unwrap()).unwrap();
    value["unexpected"] = json!(true);
    assert!(RequestEnvelope::from_json(&value.to_string()).is_err());

    let mut value: serde_json::Value =
        serde_json::from_str(&requests()[2].to_json().unwrap()).unwrap();
    value["attribution"]["unexpected"] = json!(true);
    assert!(RequestEnvelope::from_json(&value.to_string()).is_err());

    let response = ResponseEnvelope {
        attribution: attribution(),
        response: Response::Retrieve(RetrieveResponse {
            hash: "0123456789abcdef01234567".into(),
            payload: "payload".into(),
        }),
    };
    let mut value: serde_json::Value = serde_json::from_str(&response.to_json().unwrap()).unwrap();
    value["result"]["unexpected"] = json!(true);
    assert!(ResponseEnvelope::from_json(&value.to_string()).is_err());

    let response = ResponseEnvelope {
        attribution: attribution(),
        response: Response::BeforeModel(BeforeModelResponse {
            tools: vec![],
            visible_markers: vec![],
        }),
    };
    let mut value: serde_json::Value = serde_json::from_str(&response.to_json().unwrap()).unwrap();
    value["result"]["retrieve_tool"] = json!(null);
    assert!(ResponseEnvelope::from_json(&value.to_string()).is_err());
}

#[test]
fn protocol_v1_is_rejected_before_shape_validation() {
    let error = RequestEnvelope::from_json(
        r#"{"protocol_version":1,"content":"x","agent_id":"a","seam":"post_tool"}"#,
    )
    .unwrap_err();
    assert!(matches!(
        error,
        ProtocolError::UnsupportedVersion { found: 1 }
    ));
}

#[test]
fn response_operation_must_match_request() {
    let response = ResponseEnvelope {
        attribution: attribution(),
        response: Response::Retrieve(RetrieveResponse {
            hash: "0123456789abcdef01234567".into(),
            payload: "payload".into(),
        }),
    };
    assert!(matches!(
        response.ensure_operation(Operation::PostTool),
        Err(ProtocolError::OperationMismatch { .. })
    ));
}
