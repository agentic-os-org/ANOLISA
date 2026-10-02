use serde::{de::DeserializeOwned, Serialize};

use cosh_gateway_contracts::{
    capability::{
        ApprovalDecision, BrokeredOperation, CapabilityDecision, CapabilityRequest,
        CapabilityScope, ExecutionPermit, OperationDescriptor, RuntimeExecutionFence,
        WorkspaceCheckpointCreateV1,
    },
    common::{
        ActorKind, ActorRef, AuthAssurance, BoundedName, BoundedOpaque, BoundedStringError,
        BoundedText, ContentPart, ContractHeader, ContractSchema, Correlation, Digest,
        IdempotencyKey, TargetRef, WorkspaceRef, CONTRACT_SCHEMA_VERSION, MAX_OPAQUE_BYTES,
        MAX_TEXT_BYTES, RUNTIME_CONTRACT_SCHEMA_VERSION, TASK_EVENT_SCHEMA_VERSION,
    },
    error::{ContractError, ErrorCategory},
    ids::{
        ActorId, AgentSessionId, ApprovalId, CheckpointId, ExecutionId, InputRequestId,
        InstallationId, MessageId, PermitId, RequestId, RunId, RuntimeBindingId, ShellSessionId,
        TaskId, ToolUseId, TurnId,
    },
    runtime::{
        AgentRuntimeCommand, AgentRuntimeEvent, BrokeredExecutionDelivery,
        BrokeredExecutionOutcome, BrokeredExecutionRef, BrokeredOperationResult,
        BrokeredRequestAcknowledgement, RuntimeCommandEnvelope, RuntimeEventEnvelope,
        RuntimeInputError, RuntimeInputOption, RuntimeInputRequest, RuntimeInputResponse,
        RuntimeInputSelections, TurnOutcome, WorkspaceCheckpointCreateV1Outcome,
        WorkspaceCheckpointCreateV1Result, MAX_RUNTIME_INPUT_OPTIONS,
        MAX_RUNTIME_INPUT_REQUEST_TEXT_BYTES, MAX_RUNTIME_INPUT_SELECTIONS,
    },
    task::{
        ApprovalPolicy, CheckpointPolicy, GatewayCommandEnvelope, TaskCommand, TaskEvent,
        TaskEventEnvelope, TaskEventKind, TaskLaunchDescriptorV1, TaskLaunchSpecV1, TaskRuntime,
        TaskState, TASK_LAUNCH_SPEC_V1,
    },
};

fn digest(byte: char) -> Digest {
    Digest::parse(byte.to_string().repeat(64)).expect("test digest is canonical")
}

fn target() -> TargetRef {
    TargetRef {
        kind: BoundedName::new("ecs").expect("test name is bounded"),
        authority: BoundedName::new("local").expect("test name is bounded"),
        identifier: BoundedOpaque::new("instance-1").expect("test ID is bounded"),
    }
}

fn actor() -> ActorRef {
    ActorRef {
        actor_id: ActorId::new(),
        actor_kind: ActorKind::Human,
        issuer: BoundedName::new("local-os").expect("test issuer is bounded"),
        assurance: AuthAssurance::LocalOs,
    }
}

fn checkpoint_id() -> CheckpointId {
    CheckpointId::parse("ckp_00000000-0000-0000-0000-000000000001")
        .expect("test checkpoint ID is canonical")
}

fn header(schema: ContractSchema) -> ContractHeader {
    ContractHeader::new(
        schema,
        MessageId::new(),
        1_700_000_000_000,
        Correlation::new(InstallationId::new()),
    )
}

fn assert_schema_mismatch_rejected<T>(value: &T, wrong_schema: &str)
where
    T: Serialize + DeserializeOwned,
{
    let mut json = serde_json::to_value(value).expect("envelope serializes");
    json["header"]["schema"] = serde_json::json!(wrong_schema);
    assert!(serde_json::from_value::<T>(json).is_err());
}

#[test]
fn internal_id_types_reject_cross_parsing() {
    let task_id = TaskId::new();
    assert!(RunId::parse(task_id.as_str()).is_err());
    assert!(RequestId::parse(task_id.as_str()).is_err());
    assert_eq!(
        TaskId::parse(task_id.as_str()).expect("same ID type parses"),
        task_id
    );

    let agent_session_id = AgentSessionId::new();
    assert!(ShellSessionId::parse(agent_session_id.as_str()).is_err());

    let tool_use_id = ToolUseId::new();
    assert!(ExecutionId::parse(tool_use_id.as_str()).is_err());
    assert!(ApprovalId::parse(tool_use_id.as_str()).is_err());
}

#[test]
fn ids_serialize_as_validated_canonical_strings() {
    let task_id = TaskId::new();
    let json = serde_json::to_string(&task_id).expect("ID serializes");
    let decoded: TaskId = serde_json::from_str(&json).expect("canonical ID deserializes");
    assert_eq!(decoded, task_id);
    assert!(
        serde_json::from_str::<TaskId>("\"run_00000000-0000-0000-0000-000000000000\"").is_err()
    );
}

#[test]
fn task_command_and_event_envelopes_round_trip() {
    let command = GatewayCommandEnvelope {
        header: header(ContractSchema::GatewayCommand),
        actor: actor(),
        idempotency_key: IdempotencyKey::new("channel-message-1").expect("test key is bounded"),
        expected_task_revision: None,
        command: TaskCommand::CreateTask {
            intent: BoundedText::new("inspect disk pressure").expect("test text is bounded"),
            target: target(),
        },
    };
    let command_json = serde_json::to_string(&command).expect("command serializes");
    let command_decoded: GatewayCommandEnvelope =
        serde_json::from_str(&command_json).expect("command deserializes");
    assert_eq!(command_decoded, command);
    assert_schema_mismatch_rejected(&command, "cosh.runtime.command");

    let retry = TaskCommand::RetryRun {
        task_id: TaskId::new(),
        previous_run_id: RunId::new(),
    };
    let retry_json = serde_json::to_string(&retry).expect("retry command serializes");
    assert!(retry_json.contains("\"command\":\"retry_run\""));
    assert_eq!(
        serde_json::from_str::<TaskCommand>(&retry_json).expect("retry command deserializes"),
        retry
    );

    let event = TaskEventEnvelope {
        header: header(ContractSchema::TaskEvent),
        task_id: TaskId::new(),
        revision: 1,
        event: TaskEvent::TaskSubmitted {
            intent_digest: digest('a'),
            target: target(),
        },
    };
    let event_json = serde_json::to_string(&event).expect("event serializes");
    let event_decoded: TaskEventEnvelope =
        serde_json::from_str(&event_json).expect("event deserializes");
    assert_eq!(event_decoded, event);
    assert_eq!(event_decoded.event.kind(), TaskEventKind::TaskSubmitted);
    assert_schema_mismatch_rejected(&event, "cosh.gateway.command");

    assert_eq!(
        serde_json::to_string(&TaskState::WaitingApproval).expect("state serializes"),
        "\"waiting_approval\""
    );
    assert_eq!(
        serde_json::to_string(&TaskState::WaitingInput).expect("state serializes"),
        "\"waiting_input\""
    );
}

#[test]
fn runtime_and_capability_contracts_round_trip() {
    let task_id = TaskId::new();
    let run_id = RunId::new();
    let request_id = RequestId::new();
    let request = CapabilityRequest {
        request_id: request_id.clone(),
        task_id: task_id.clone(),
        run_id: run_id.clone(),
        actor: actor(),
        target: target(),
        operation: OperationDescriptor {
            namespace: BoundedName::new("process").expect("test name is bounded"),
            name: BoundedName::new("spawn").expect("test name is bounded"),
            arguments_digest: digest('b'),
        },
        operation_digest: digest('e'),
        requested_scope: CapabilityScope {
            resource: BoundedName::new("host").expect("test name is bounded"),
            access: BoundedName::new("execute").expect("test name is bounded"),
        },
        input_digest: digest('c'),
        expires_at_ms: 1_700_000_001_000,
    };
    let permit = ExecutionPermit {
        permit_id: PermitId::new(),
        request_id,
        actor_id: request.actor.actor_id.clone(),
        approval_id: Some(ApprovalId::new()),
        task_id,
        run_id: run_id.clone(),
        execution_id: ExecutionId::new(),
        target: target(),
        target_identity_digest: digest('f'),
        runtime_fence: RuntimeExecutionFence {
            binding_id: RuntimeBindingId::new(),
            runtime_generation: 3,
            lease_generation: 5,
            lease_revision: 8,
        },
        operation_digest: digest('d'),
        input_digest: request.input_digest.clone(),
        policy_revision: 7,
        valid_until_ms: 1_700_000_001_000,
        single_use: true,
    };
    let decision = CapabilityDecision::Permit { permit };
    let decision_json = serde_json::to_string(&decision).expect("decision serializes");
    let decision_decoded: CapabilityDecision =
        serde_json::from_str(&decision_json).expect("decision deserializes");
    assert_eq!(decision_decoded, decision);
    assert!(decision_json.contains("target_identity_digest"));
    assert!(decision_json.contains("runtime_fence"));

    let runtime = RuntimeCommandEnvelope {
        header: header(ContractSchema::RuntimeCommand),
        command: AgentRuntimeCommand::Prompt {
            run_id,
            turn_id: TurnId::new(),
            input: vec![ContentPart::Text {
                text: BoundedText::new("continue").expect("test text is bounded"),
            }],
        },
    };
    let runtime_json = serde_json::to_string(&runtime).expect("Runtime command serializes");
    let runtime_decoded: RuntimeCommandEnvelope =
        serde_json::from_str(&runtime_json).expect("Runtime command deserializes");
    assert_eq!(runtime_decoded, runtime);
    assert_schema_mismatch_rejected(&runtime, "cosh.runtime.event");

    let runtime_event = RuntimeEventEnvelope {
        header: header(ContractSchema::RuntimeEvent),
        binding_id: RuntimeBindingId::new(),
        sequence: 1,
        event: AgentRuntimeEvent::Completed {
            turn_id: TurnId::new(),
            outcome: TurnOutcome::Completed,
        },
    };
    let runtime_event_json =
        serde_json::to_string(&runtime_event).expect("Runtime event serializes");
    let runtime_event_decoded: RuntimeEventEnvelope =
        serde_json::from_str(&runtime_event_json).expect("Runtime event deserializes");
    assert_eq!(runtime_event_decoded, runtime_event);
    assert_schema_mismatch_rejected(&runtime_event, "cosh.task.event");

    assert_eq!(ApprovalDecision::Approve, ApprovalDecision::Approve);
    assert_eq!(request.task_id.as_str().split('_').next(), Some("tsk"));
}

#[test]
fn contract_header_version_is_independent_and_fail_closed() {
    let supported = header(ContractSchema::GatewayCommand);
    assert_eq!(supported.schema_version, CONTRACT_SCHEMA_VERSION);

    let runtime = header(ContractSchema::RuntimeCommand);
    assert_eq!(runtime.schema_version, RUNTIME_CONTRACT_SCHEMA_VERSION);

    let task_event = header(ContractSchema::TaskEvent);
    assert_eq!(task_event.schema_version, TASK_EVENT_SCHEMA_VERSION);

    let mut value = serde_json::to_value(supported).expect("header serializes");
    value["schema_version"] = serde_json::json!(CONTRACT_SCHEMA_VERSION + 1);
    assert!(serde_json::from_value::<ContractHeader>(value).is_err());

    let mut old_task_event =
        serde_json::to_value(task_event).expect("Task event header serializes");
    old_task_event["schema_version"] =
        serde_json::json!(TASK_EVENT_SCHEMA_VERSION.saturating_sub(1));
    assert!(serde_json::from_value::<ContractHeader>(old_task_event).is_err());

    let mut legacy_runtime = serde_json::to_value(runtime).expect("Runtime header serializes");
    legacy_runtime["schema_version"] = serde_json::json!(2);
    assert!(serde_json::from_value::<ContractHeader>(legacy_runtime).is_err());
}

#[test]
fn runtime_v4_input_exchange_is_exact_bounded_and_round_trips() {
    assert_eq!(RUNTIME_CONTRACT_SCHEMA_VERSION, 4);
    let run_id = RunId::new();
    let turn_id = TurnId::new();
    let request_id = InputRequestId::new();
    let request = RuntimeInputRequest::new(
        request_id.clone(),
        run_id.clone(),
        turn_id.clone(),
        Some(ToolUseId::new()),
        BoundedText::new("Choose a branch").unwrap(),
        vec![
            RuntimeInputOption::new(
                BoundedText::new("main").unwrap(),
                Some(BoundedText::new("Use the default branch").unwrap()),
            ),
            RuntimeInputOption::new(BoundedText::new("release").unwrap(), None),
        ],
        true,
        false,
    )
    .unwrap();
    let event = AgentRuntimeEvent::InputRequested {
        request: request.clone(),
    };
    let event_value = serde_json::to_value(&event).unwrap();
    assert_eq!(event_value["event"], "input_requested");
    assert_eq!(event_value["request"]["request_id"], request_id.as_str());
    assert_eq!(event_value["request"]["run_id"], run_id.as_str());
    assert_eq!(event_value["request"]["turn_id"], turn_id.as_str());
    assert_eq!(
        serde_json::from_value::<AgentRuntimeEvent>(event_value).unwrap(),
        event
    );

    let command = AgentRuntimeCommand::ResolveInput {
        request_id,
        run_id,
        turn_id,
        response: RuntimeInputResponse::Options {
            selections: RuntimeInputSelections::new(vec![1]).unwrap(),
        },
    };
    let value = serde_json::to_value(&command).unwrap();
    assert_eq!(value["command"], "resolve_input");
    assert_eq!(value["response"]["response"], "options");
    assert_eq!(
        serde_json::from_value::<AgentRuntimeCommand>(value).unwrap(),
        command
    );
}

#[test]
fn runtime_input_bounds_fail_closed_during_construction_and_decode() {
    let option = |label: String| RuntimeInputOption::new(BoundedText::new(label).unwrap(), None);
    let request = |options, allow_free_text, multi_select| {
        RuntimeInputRequest::new(
            InputRequestId::new(),
            RunId::new(),
            TurnId::new(),
            None,
            BoundedText::new("question").unwrap(),
            options,
            allow_free_text,
            multi_select,
        )
    };

    assert_eq!(
        request(Vec::new(), false, false),
        Err(RuntimeInputError::NoAnswerMode)
    );
    assert_eq!(
        request(vec![option("only".to_string())], true, true),
        Err(RuntimeInputError::InvalidMultiSelect)
    );
    assert!(matches!(
        request(
            (0..=MAX_RUNTIME_INPUT_OPTIONS)
                .map(|index| option(format!("option-{index}")))
                .collect(),
            true,
            false,
        ),
        Err(RuntimeInputError::TooManyOptions { .. })
    ));
    assert_eq!(
        request(
            vec![
                option("duplicate".to_string()),
                option("duplicate".to_string())
            ],
            true,
            false,
        ),
        Err(RuntimeInputError::DuplicateOption)
    );
    let large_options = (0..4)
        .map(|index| {
            let mut label = "x".repeat(MAX_TEXT_BYTES);
            label.replace_range(..1, &index.to_string());
            option(label)
        })
        .collect();
    assert!(matches!(
        request(large_options, true, false),
        Err(RuntimeInputError::RequestTextTooLarge {
            max_bytes: MAX_RUNTIME_INPUT_REQUEST_TEXT_BYTES
        })
    ));

    assert_eq!(
        RuntimeInputSelections::new(Vec::new()),
        Err(RuntimeInputError::EmptySelection)
    );
    assert_eq!(
        RuntimeInputSelections::new(vec![0, 0]),
        Err(RuntimeInputError::DuplicateSelection)
    );
    assert!(matches!(
        RuntimeInputSelections::new(
            (0..=u16::try_from(MAX_RUNTIME_INPUT_SELECTIONS).unwrap()).collect()
        ),
        Err(RuntimeInputError::TooManySelections { .. })
    ));

    let mut oversized =
        serde_json::to_value(request(vec![option("valid".to_string())], false, false).unwrap())
            .unwrap();
    oversized["options"] = serde_json::Value::Array(
        (0..=MAX_RUNTIME_INPUT_OPTIONS)
            .map(|index| serde_json::json!({"label": format!("option-{index}")}))
            .collect(),
    );
    assert!(serde_json::from_value::<RuntimeInputRequest>(oversized).is_err());
}

#[test]
fn brokered_checkpoint_operation_has_a_minimal_golden_shape() {
    let operation = BrokeredOperation::WorkspaceCheckpointCreateV1(WorkspaceCheckpointCreateV1 {
        checkpoint_id: checkpoint_id(),
    });

    let value = serde_json::to_value(&operation).expect("brokered operation serializes");
    assert_eq!(
        value,
        serde_json::json!({
            "operation": "workspace_checkpoint_create_v1",
            "input": {
                "checkpoint_id": "ckp_00000000-0000-0000-0000-000000000001"
            }
        })
    );
    assert_eq!(
        serde_json::from_value::<BrokeredOperation>(value)
            .expect("golden brokered operation deserializes"),
        operation
    );
}

#[test]
fn brokered_checkpoint_input_rejects_runtime_owned_execution_options() {
    let forbidden = ["path", "socket", "message", "metadata", "pin", "timeout"];

    for field in forbidden {
        let mut value = serde_json::json!({
            "operation": "workspace_checkpoint_create_v1",
            "input": {
                "checkpoint_id": "ckp_00000000-0000-0000-0000-000000000001"
            }
        });
        value["input"][field] = serde_json::json!("runtime-controlled");
        assert!(
            serde_json::from_value::<BrokeredOperation>(value).is_err(),
            "field {field} must fail closed"
        );
    }
}

#[test]
fn brokered_checkpoint_runtime_event_round_trips_without_an_authority_override() {
    let task_id = TaskId::new();
    let run_id = RunId::new();
    let event = AgentRuntimeEvent::BrokeredExecutionRequested {
        turn_id: TurnId::new(),
        tool_use_id: Some(ToolUseId::new()),
        summary: cosh_gateway_contracts::runtime::ToolSummary {
            name: BoundedName::new("workspace_checkpoint_create_v1")
                .expect("test operation name is bounded"),
            summary: BoundedText::new("Create a workspace checkpoint")
                .expect("test summary is bounded"),
        },
        request: CapabilityRequest {
            request_id: RequestId::new(),
            task_id,
            run_id,
            actor: actor(),
            target: target(),
            operation: OperationDescriptor {
                namespace: BoundedName::new("workspace_checkpoint")
                    .expect("test namespace is bounded"),
                name: BoundedName::new("create_v1").expect("test name is bounded"),
                arguments_digest: digest('1'),
            },
            operation_digest: digest('2'),
            requested_scope: CapabilityScope {
                resource: BoundedName::new("bound_workspace").expect("test resource is bounded"),
                access: BoundedName::new("checkpoint_create").expect("test access is bounded"),
            },
            input_digest: digest('3'),
            expires_at_ms: 1_700_000_001_000,
        },
        operation: BrokeredOperation::WorkspaceCheckpointCreateV1(WorkspaceCheckpointCreateV1 {
            checkpoint_id: checkpoint_id(),
        }),
    };

    let value = serde_json::to_value(&event).expect("brokered event serializes");
    assert_eq!(value["event"], "brokered_execution_requested");
    assert!(value.get("authority").is_none());
    assert_eq!(
        serde_json::from_value::<AgentRuntimeEvent>(value.clone())
            .expect("brokered event deserializes"),
        event
    );

    let mut unknown_operation = value.clone();
    unknown_operation["operation"]["operation"] =
        serde_json::json!("workspace_checkpoint_restore_v1");
    assert!(serde_json::from_value::<AgentRuntimeEvent>(unknown_operation).is_err());

    let mut invalid_provider_native = value;
    invalid_provider_native["event"] = serde_json::json!("execution_permission_requested");
    invalid_provider_native
        .as_object_mut()
        .expect("test event is an object")
        .remove("operation");
    invalid_provider_native["authority"] = serde_json::json!("cosh_brokered");
    assert!(serde_json::from_value::<AgentRuntimeEvent>(invalid_provider_native).is_err());
}

#[test]
fn brokered_execution_ref_round_trips_and_rejects_unknown_fields() {
    let reference = BrokeredExecutionRef {
        binding_id: RuntimeBindingId::new(),
        runtime_generation: 2,
        event_sequence: 3,
        run_id: RunId::new(),
        turn_id: TurnId::new(),
        tool_use_id: Some(ToolUseId::new()),
        request_id: RequestId::new(),
        operation: BrokeredOperation::WorkspaceCheckpointCreateV1(WorkspaceCheckpointCreateV1 {
            checkpoint_id: checkpoint_id(),
        }),
    };
    let mut value = serde_json::to_value(&reference).expect("brokered reference serializes");
    assert_eq!(
        serde_json::from_value::<BrokeredExecutionRef>(value.clone())
            .expect("brokered reference deserializes"),
        reference
    );

    value["permit_id"] = serde_json::json!(PermitId::new());
    assert!(serde_json::from_value::<BrokeredExecutionRef>(value).is_err());
}

#[test]
fn brokered_acknowledgement_and_result_commands_round_trip_without_permits() {
    let request_id = RequestId::new();
    let acknowledgement = AgentRuntimeCommand::AcknowledgeBrokeredRequest {
        acknowledgement: BrokeredRequestAcknowledgement {
            request_id: request_id.clone(),
            approval_id: ApprovalId::new(),
        },
    };
    let acknowledgement_json =
        serde_json::to_string(&acknowledgement).expect("acknowledgement serializes");
    assert_eq!(
        serde_json::from_str::<AgentRuntimeCommand>(&acknowledgement_json)
            .expect("acknowledgement deserializes"),
        acknowledgement
    );
    assert!(!acknowledgement_json.contains("permit"));
    assert!(!acknowledgement_json.contains("decision"));
    let mut acknowledgement_with_permit =
        serde_json::to_value(&acknowledgement).expect("acknowledgement serializes");
    acknowledgement_with_permit["acknowledgement"]["permit_id"] =
        serde_json::json!(PermitId::new());
    assert!(
        serde_json::from_value::<AgentRuntimeCommand>(acknowledgement_with_permit).is_err(),
        "a brokered acknowledgement must reject permit authority"
    );

    let delivery = AgentRuntimeCommand::DeliverBrokeredResult {
        delivery: BrokeredExecutionDelivery {
            request_id,
            outcome: BrokeredExecutionOutcome::Succeeded {
                execution_id: ExecutionId::new(),
                result: BrokeredOperationResult::WorkspaceCheckpointCreateV1(
                    WorkspaceCheckpointCreateV1Result {
                        checkpoint_id: checkpoint_id(),
                        outcome: WorkspaceCheckpointCreateV1Outcome::Created {
                            snapshot_id: BoundedOpaque::new("snapshot-1")
                                .expect("test snapshot ID is bounded"),
                        },
                    },
                ),
            },
        },
    };
    let delivery_json = serde_json::to_string(&delivery).expect("delivery serializes");
    assert_eq!(
        serde_json::from_str::<AgentRuntimeCommand>(&delivery_json).expect("delivery deserializes"),
        delivery
    );
    assert!(!delivery_json.contains("permit"));

    let mut oversized_result = serde_json::to_value(&delivery).expect("brokered result serializes");
    oversized_result["delivery"]["outcome"]["result"]["result"]["outcome"]["snapshot_id"] =
        serde_json::json!("x".repeat(MAX_OPAQUE_BYTES + 1));
    assert!(serde_json::from_value::<AgentRuntimeCommand>(oversized_result).is_err());
}

#[test]
fn brokered_denial_delivery_is_typed_and_bounded() {
    let delivery = BrokeredExecutionDelivery {
        request_id: RequestId::new(),
        outcome: BrokeredExecutionOutcome::Denied {
            code: cosh_gateway_contracts::capability::DenialCode::ApprovalDenied,
            safe_message: BoundedText::new("operator denied checkpoint")
                .expect("test denial is bounded"),
        },
    };
    let mut value = serde_json::to_value(&delivery).expect("denial delivery serializes");
    assert_eq!(value["outcome"]["outcome"], "denied");
    assert_eq!(
        serde_json::from_value::<BrokeredExecutionDelivery>(value.clone())
            .expect("denial delivery deserializes"),
        delivery
    );

    value["outcome"]["safe_message"] = serde_json::json!("x".repeat(MAX_TEXT_BYTES + 1));
    assert!(serde_json::from_value::<BrokeredExecutionDelivery>(value).is_err());
}

#[test]
fn contract_errors_are_bounded_during_construction_and_deserialization() {
    let error = ContractError::new(
        "runtime_unavailable",
        ErrorCategory::RuntimeUnavailable,
        true,
        "runtime is temporarily unavailable",
    )
    .expect("test error is bounded");
    let json = serde_json::to_string(&error).expect("error serializes");
    let decoded: ContractError = serde_json::from_str(&json).expect("error deserializes");
    assert_eq!(decoded, error);

    assert_eq!(
        BoundedText::new("x".repeat(MAX_TEXT_BYTES + 1)),
        Err(BoundedStringError::TooLong {
            max_bytes: MAX_TEXT_BYTES
        })
    );

    let mut oversized = serde_json::to_value(error).expect("error serializes");
    oversized["safe_message"] = serde_json::json!("x".repeat(MAX_TEXT_BYTES + 1));
    assert!(serde_json::from_value::<ContractError>(oversized).is_err());
}

#[test]
fn task_launch_spec_v1_is_strict_and_round_trips() {
    let launch = TaskLaunchSpecV1::new(
        BoundedText::new("summarize the repository").expect("goal is bounded"),
        TaskRuntime::Codex,
        WorkspaceRef {
            scope_digest: digest('f'),
            display_name: Some(BoundedText::new("workspace").expect("label is bounded")),
        },
        CheckpointPolicy::Auto,
        ApprovalPolicy::AllowAll,
    );
    let value = serde_json::to_value(&launch).expect("launch serializes");
    assert_eq!(value["schema_version"], TASK_LAUNCH_SPEC_V1);
    assert_eq!(
        serde_json::from_value::<TaskLaunchSpecV1>(value.clone()).expect("launch deserializes"),
        launch
    );

    let mut unknown_version = value.clone();
    unknown_version["schema_version"] = serde_json::json!(2);
    assert!(serde_json::from_value::<TaskLaunchSpecV1>(unknown_version).is_err());

    let mut unknown_field = value;
    unknown_field["provider"] = serde_json::json!("client-selected");
    assert!(serde_json::from_value::<TaskLaunchSpecV1>(unknown_field).is_err());
}

#[test]
fn task_launch_descriptor_v1_omits_the_private_goal() {
    let launch = TaskLaunchSpecV1::new(
        BoundedText::new("private repository goal").expect("goal is bounded"),
        TaskRuntime::Core,
        WorkspaceRef {
            scope_digest: digest('e'),
            display_name: None,
        },
        CheckpointPolicy::Off,
        ApprovalPolicy::AllowAll,
    );
    let descriptor = TaskLaunchDescriptorV1::from_spec(digest('d'), &launch);
    let value = serde_json::to_value(&descriptor).expect("descriptor serializes");

    assert_eq!(value["schema_version"], TASK_LAUNCH_SPEC_V1);
    assert_eq!(value["goal_digest"], digest('d').as_str());
    assert!(value.get("goal").is_none());
    assert_eq!(
        serde_json::from_value::<TaskLaunchDescriptorV1>(value.clone())
            .expect("descriptor deserializes"),
        descriptor
    );

    let mut unknown_version = value;
    unknown_version["schema_version"] = serde_json::json!(2);
    assert!(serde_json::from_value::<TaskLaunchDescriptorV1>(unknown_version).is_err());
}
