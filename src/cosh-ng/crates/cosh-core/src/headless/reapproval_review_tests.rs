use super::*;
use crate::cli::ExecutionProfile;
use crate::provider::{mock::MockProvider, GenerateEvent};
use crate::tool::ToolRegistry;
use clap::Parser;

fn tool_call(name: &str, arguments: serde_json::Value) -> Vec<GenerateEvent> {
    vec![
        GenerateEvent::ToolCallStart {
            index: 0,
            id: "effect-call".into(),
            name: name.into(),
        },
        GenerateEvent::ToolCallDelta {
            index: 0,
            arguments_delta: arguments.to_string(),
        },
        GenerateEvent::ToolCallEnd { index: 0 },
        GenerateEvent::MessageEnd,
    ]
}

fn fixture(
    workspace: &std::path::Path,
    config: CoreConfig,
    profile: ExecutionProfile,
    tools: ToolRegistry,
    calls: Vec<Vec<GenerateEvent>>,
) -> (SessionRuntime, CoshCore) {
    let store =
        SessionStore::for_test_workspace(workspace.join("sessions").to_str().unwrap(), workspace)
            .unwrap();
    let workspace_scope = store.workspace_scope().to_string();
    let record = PersistedSession::new(
        ProviderSessionId::new(),
        workspace_scope.clone(),
        "mock-model".into(),
        Vec::new(),
    );
    let runtime = SessionRuntime {
        store: Some(store),
        live_lease: None,
        workspace_scope,
        record,
        auto_persist: true,
        resumed: false,
        notice: PersistenceNotice::default(),
    };
    let responses = calls
        .into_iter()
        .flat_map(|events| [events, vec![GenerateEvent::MessageEnd]])
        .collect();
    let snapshot =
        RuntimeSnapshot::bootstrap(RuntimeGeneration::healthy(1, "test"), Arc::new(tools));
    let engine = CoshCore::new_with_snapshot_and_session_id(
        config,
        Box::new(MockProvider::new(responses)),
        snapshot,
        runtime.record.session_id.to_string(),
        workspace.to_path_buf(),
        SessionWorkspace::new(workspace),
        profile,
    );
    (runtime, engine)
}

fn response(body: serde_json::Value) -> String {
    serde_json::json!({"type":"control_response","response":{"subtype":"success","request_id":"req-0","response":body}}).to_string()+"\n"
}

async fn turn(engine: &mut CoshCore, input: &str) -> Result<(), String> {
    let mut lines = BufReader::new(input.as_bytes()).lines();
    engine
        .handle_user_message("continue current task", &mut lines, &mut Vec::new())
        .await
        .map(|_| ())
}

fn assert_durable_pending(runtime: &SessionRuntime, engine: &CoshCore) {
    assert!(engine.failed_side_effect_pending_approval);
    assert!(engine
        .reapproval_checkpoint
        .as_ref()
        .unwrap()
        .pending()
        .unwrap());
    assert!(
        runtime
            .store
            .as_ref()
            .unwrap()
            .load(&runtime.record.session_id)
            .unwrap()
            .failed_side_effect_pending_approval
    );
}

#[tokio::test]
async fn malformed_host_shell_completion_keeps_durable_reapproval() {
    let dir = tempfile::tempdir().unwrap();
    let mut config = CoreConfig::default();
    config.agent.approval_mode = ApprovalMode::Auto;
    let (mut runtime, mut engine) = fixture(
        dir.path(),
        config,
        ExecutionProfile::Legacy,
        ToolRegistry::with_defaults_for_test(),
        vec![
            tool_call(
                "shell",
                serde_json::json!({"command":"printf host-effect > effect"}),
            ),
            tool_call(
                "write_file",
                serde_json::json!({"path":"effect","content":"unapproved overwrite"}),
            ),
        ],
    );
    runtime.persist(&mut engine).unwrap();
    std::fs::write(dir.path().join("effect"), "host-effect").unwrap();
    turn(
        &mut engine,
        &response(serde_json::json!({"behavior":"host_executed_shell"})),
    )
    .await
    .unwrap();
    runtime.persist(&mut engine).unwrap();
    let deny = serde_json::json!({"type":"control_response","response":{"subtype":"success","request_id":"req-1","response":{"behavior":"deny"}}}).to_string()+"\n";
    turn(&mut engine, &deny).await.unwrap();
    assert_eq!(
        std::fs::read_to_string(dir.path().join("effect")).unwrap(),
        "host-effect",
        "malformed completion must not permit the next automatic write"
    );
    assert_durable_pending(&runtime, &engine);
}

#[tokio::test]
async fn malformed_checkpoint_completion_keeps_durable_reapproval() {
    for body in [
        serde_json::json!({"behavior":"host_executed_checkpoint_create"}),
        serde_json::json!({"behavior":"host_executed_checkpoint_create","checkpointResult":{"checkpoint_id":"invalid","outcome":{"status":"created","snapshot_id":"snap"}}}),
        serde_json::json!({"behavior":"host_executed_checkpoint_error","checkpointError":{"outcome":"uncertain","code":"","message":"unconfirmed"}}),
    ] {
        let dir = tempfile::tempdir().unwrap();
        let (mut runtime, mut engine) = fixture(
            dir.path(),
            CoreConfig::default(),
            ExecutionProfile::GatewayBrokeredCheckpointV1,
            ToolRegistry::gateway_brokered_checkpoint_v1(),
            vec![tool_call(
                "workspace_checkpoint_create",
                serde_json::json!({}),
            )],
        );
        runtime.persist(&mut engine).unwrap();
        turn(&mut engine, &response(body)).await.unwrap();
        runtime.persist(&mut engine).unwrap();
        assert_durable_pending(&runtime, &engine);
    }
}

#[tokio::test]
async fn execution_failure_survives_required_bypass_audit_error_and_persist() {
    for post_hook_denies in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let mut config = CoreConfig::default();
        config.agent.approval_mode = ApprovalMode::Trust;
        config.hooks.enabled = true;
        config.hooks.post_tool_use_failure = vec![crate::config::HookDefinition {
            command: r#"printf '%s\n' '{"hookSpecificOutput":{"sandbox_bypass_request":{"original_command":"printf retry > effect","reason":"test failed effect"}}}'"#.into(),
            name: Some("bypass-audit-regression".into()), matcher: None, timeout: Some(10_000), sequential: None, fail_open: false, env: Default::default(),
        }];
        if post_hook_denies {
            config.hooks.post_tool_use = vec![crate::config::HookDefinition {
                command:
                    r#"printf '%s\n' '{"decision":"block","reason":"test post-execution failure"}'"#
                        .into(),
                name: Some("post-execution-block".into()),
                ..config.hooks.post_tool_use_failure[0].clone()
            }];
        }
        let (mut runtime, mut engine) = fixture(
            dir.path(),
            config,
            ExecutionProfile::Legacy,
            ToolRegistry::with_defaults_for_test(),
            vec![tool_call(
                "shell",
                serde_json::json!({"command":if post_hook_denies {"printf partial > effect"} else {"printf partial > effect; exit 23"}}),
            )],
        );
        runtime.persist(&mut engine).unwrap();
        engine.audit = crate::audit::CoreAuditRecorder::test_capture_except(
            &engine.session_id,
            cosh_types::audit::KnownAuditEventType::ApprovalResolved,
        );
        let error = turn(
            &mut engine,
            &response(serde_json::json!({"behavior":"allow"})),
        )
        .await
        .unwrap_err();
        assert!(error.contains("audit"), "{error}");
        assert_eq!(
            std::fs::read_to_string(dir.path().join("effect")).unwrap(),
            "partial"
        );
        runtime.persist(&mut engine).unwrap();
        assert_durable_pending(&runtime, &engine);
    }
}

fn publish_without_advancing_runtime(runtime: &mut SessionRuntime) {
    let store = runtime.store.as_ref().unwrap();
    runtime.live_lease = store
        .acquire_live_lease(&runtime.record.session_id)
        .unwrap()
        .map(Arc::new);
    // Recreates the caller-visible state after rename succeeds but directory sync fails.
    let mut published = runtime.record.clone();
    store.persist(&mut published).unwrap();
    assert!(!runtime.record.was_loaded);
    assert_eq!(runtime.record.generation, 0);
    assert_eq!(published.generation, 1);
}

#[tokio::test]
async fn retained_published_lease_guards_effect_after_failed_commit() {
    let dir = tempfile::tempdir().unwrap();
    let mut config = CoreConfig::default();
    config.agent.approval_mode = ApprovalMode::Trust;
    let (mut runtime, mut engine) = fixture(
        dir.path(),
        config,
        ExecutionProfile::Legacy,
        ToolRegistry::with_defaults_for_test(),
        vec![tool_call(
            "shell",
            serde_json::json!({"command":"printf partial > effect; exit 23"}),
        )],
    );
    publish_without_advancing_runtime(&mut runtime);
    assert!(matches!(
        runtime.persist(&mut engine),
        Err(SessionError::Conflict { .. })
    ));
    assert!(runtime.live_lease.is_some());
    turn(&mut engine, "").await.unwrap();
    assert_eq!(
        std::fs::read_to_string(dir.path().join("effect")).unwrap(),
        "partial"
    );
    assert!(engine
        .reapproval_checkpoint
        .as_ref()
        .expect("published identity must bind despite rejected commit")
        .pending()
        .unwrap());
    assert!(
        !runtime
            .store
            .as_ref()
            .unwrap()
            .load(&runtime.record.session_id)
            .unwrap()
            .failed_side_effect_pending_approval,
        "the old false envelope makes the activity guard essential"
    );
}

#[tokio::test]
// Global telemetry paths must stay isolated across this interactive await.
#[allow(clippy::await_holding_lock)]
async fn invalid_retained_lease_stops_interactive_effect_before_execution() {
    let _environment_lock = crate::sls::tests::ENV_TEST_MUTEX.lock().unwrap();
    let telemetry = tempfile::tempdir().unwrap();
    let disabled = telemetry.path().join("disabled");
    std::fs::write(&disabled, "").unwrap();
    let _installation_id = crate::sls::tests::EnvVarGuard::set(
        "COSH_INSTALLATION_ID_PATH",
        telemetry.path().join("installation_id").to_str().unwrap(),
    );
    let _disabled = crate::sls::tests::EnvVarGuard::set(
        "COSH_TELEMETRY_DISABLED_PATH",
        disabled.to_str().unwrap(),
    );
    let dir = tempfile::tempdir().unwrap();
    let mut config = CoreConfig::default();
    config.agent.approval_mode = ApprovalMode::Auto;
    let (mut runtime, mut engine) = fixture(
        dir.path(),
        config,
        ExecutionProfile::Legacy,
        ToolRegistry::with_defaults_for_test(),
        vec![tool_call(
            "write_file",
            serde_json::json!({"path":"effect","content":"must not execute"}),
        )],
    );
    publish_without_advancing_runtime(&mut runtime);
    let store = runtime.store.as_ref().unwrap();
    let envelope = store.session_file(&runtime.record.session_id);
    let lease_path = envelope
        .parent()
        .unwrap()
        .join(format!(".{}.live", runtime.record.session_id));
    std::fs::rename(&lease_path, lease_path.with_extension("old")).unwrap();
    std::fs::write(&lease_path, [0]).unwrap();
    assert!(runtime.persist(&mut engine).is_err());
    let args = CliArgs::try_parse_from(["cosh-core", "--headless"]).unwrap();
    let live = crate::registry::LiveExtensionRuntime::new(
        engine.extension_generation.clone(),
        false,
        false,
        None,
    );
    let mut extensions = HeadlessExtensionRuntime {
        manager: None,
        live: &live,
    };
    let mut protocol = ProtocolState::new(ExecutionProfile::Legacy);
    let mut lines = BufReader::new(&b""[..]).lines();
    let mut output = Vec::new();
    let result = process_input_line(
        r#"{"type":"user","message":{"role":"user","content":"perform effect"}}"#,
        &mut engine,
        &mut lines,
        &mut output,
        &args,
        &mut runtime,
        &mut extensions,
        &mut protocol,
    )
    .await;
    assert!(
        !dir.path().join("effect").exists(),
        "invalid retained lease allowed an automatic side effect"
    );
    assert!(
        matches!(result, InputLineResult::Shutdown),
        "checkpoint binding failure must stop the interactive process"
    );
    let old_lease = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(lease_path.with_extension("old"))
        .unwrap();
    assert!(rustix::fs::flock(
        &old_lease,
        rustix::fs::FlockOperation::NonBlockingLockExclusive
    )
    .is_err());
    engine.shutdown_extension_runtime().await;
    drop(engine);
    drop(runtime);
    rustix::fs::flock(
        &old_lease,
        rustix::fs::FlockOperation::NonBlockingLockExclusive,
    )
    .unwrap();
}

#[tokio::test]
async fn correlated_denial_restores_only_a_new_checkpoint() {
    for was_pending in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let mut config = CoreConfig::default();
        config.agent.approval_mode = ApprovalMode::Auto;
        let (mut runtime, mut engine) = fixture(
            dir.path(),
            config,
            ExecutionProfile::Legacy,
            ToolRegistry::with_defaults_for_test(),
            vec![tool_call(
                "shell",
                serde_json::json!({"command":"printf forbidden > effect"}),
            )],
        );
        runtime.persist(&mut engine).unwrap();
        engine.failed_side_effect_pending_approval = was_pending;
        engine
            .reapproval_checkpoint
            .as_ref()
            .unwrap()
            .set_pending(was_pending)
            .unwrap();
        let wrong = serde_json::json!({"type":"control_response","response":{"subtype":"success","request_id":"wrong-request","response":{"behavior":"deny"}}}).to_string()+"\n";
        let input = wrong + &response(serde_json::json!({"behavior":"deny"}));
        turn(&mut engine, &input).await.unwrap();
        assert!(!dir.path().join("effect").exists());
        assert_eq!(engine.failed_side_effect_pending_approval, was_pending);
        assert_eq!(
            engine
                .reapproval_checkpoint
                .as_ref()
                .unwrap()
                .pending()
                .unwrap(),
            was_pending
        );
        runtime.persist(&mut engine).unwrap();
        assert_eq!(
            runtime
                .store
                .as_ref()
                .unwrap()
                .load(&runtime.record.session_id)
                .unwrap()
                .failed_side_effect_pending_approval,
            was_pending
        );
    }
}

#[tokio::test]
async fn unconfirmed_completion_marks_pending_before_required_audit_failure() {
    for body in [
        serde_json::json!({"behavior":"host_executed_shell"}),
        serde_json::json!({"behavior":"unknown-response"}),
    ] {
        let dir = tempfile::tempdir().unwrap();
        let (mut runtime, mut engine) = fixture(
            dir.path(),
            CoreConfig::default(),
            ExecutionProfile::Legacy,
            ToolRegistry::with_defaults_for_test(),
            vec![tool_call(
                "shell",
                serde_json::json!({"command":"printf forbidden > effect"}),
            )],
        );
        runtime.persist(&mut engine).unwrap();
        engine.audit = crate::audit::CoreAuditRecorder::test_capture_except(
            &engine.session_id,
            cosh_types::audit::KnownAuditEventType::ApprovalResolved,
        );
        let error = turn(&mut engine, &response(body)).await.unwrap_err();
        assert!(error.contains("audit"), "{error}");
        assert!(!dir.path().join("effect").exists());
        runtime.persist(&mut engine).unwrap();
        assert_durable_pending(&runtime, &engine);
    }
}

struct ObserveBypassCheckpoint {
    output: Vec<u8>,
    lease_path: PathBuf,
    pending_when_bypass_sent: Option<bool>,
}

impl io::Write for ObserveBypassCheckpoint {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.output.extend_from_slice(bytes);
        if self.pending_when_bypass_sent.is_none()
            && self
                .output
                .split(|byte| *byte == b'\n')
                .filter_map(|line| serde_json::from_slice::<serde_json::Value>(line).ok())
                .any(|message| {
                    message["type"] == "control_request" && message["request_id"] == "req-1"
                })
        {
            self.pending_when_bypass_sent = Some(std::fs::read(&self.lease_path)? == [1]);
        }
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[tokio::test]
async fn denial_then_bypass_keeps_checkpoint_until_both_denials() {
    for was_pending in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let mut config = CoreConfig::default();
        config.agent.approval_mode = ApprovalMode::Auto;
        config.hooks.enabled = true;
        config.hooks.post_tool_use_failure = vec![crate::config::HookDefinition {
            command: r#"printf '%s\n' '{"hookSpecificOutput":{"sandbox_bypass_request":{"original_command":"printf forbidden > effect","reason":"test denied bypass"}}}'"#.into(),
            name: Some("denied-bypass-regression".into()), matcher: None, timeout: Some(10_000), sequential: None, fail_open: false, env: Default::default(),
        }];
        let (mut runtime, mut engine) = fixture(
            dir.path(),
            config,
            ExecutionProfile::Legacy,
            ToolRegistry::with_defaults_for_test(),
            vec![tool_call(
                "shell",
                serde_json::json!({"command":"printf forbidden > effect"}),
            )],
        );
        runtime.persist(&mut engine).unwrap();
        engine.failed_side_effect_pending_approval = was_pending;
        engine
            .reapproval_checkpoint
            .as_ref()
            .unwrap()
            .set_pending(was_pending)
            .unwrap();
        let store = runtime.store.as_ref().unwrap();
        let envelope = store.session_file(&runtime.record.session_id);
        let mut writer = ObserveBypassCheckpoint {
            output: Vec::new(),
            lease_path: envelope
                .parent()
                .unwrap()
                .join(format!(".{}.live", runtime.record.session_id)),
            pending_when_bypass_sent: None,
        };
        let second = serde_json::json!({"type":"control_response","response":{"subtype":"success","request_id":"req-1","response":{"behavior":"deny"}}}).to_string()+"\n";
        let input = response(serde_json::json!({"behavior":"deny"})) + &second;
        let mut lines = BufReader::new(input.as_bytes()).lines();
        engine
            .handle_user_message("perform effect", &mut lines, &mut writer)
            .await
            .unwrap();
        assert_eq!(
            writer.pending_when_bypass_sent,
            Some(true),
            "the second Host handoff must remain armed after the first deny"
        );
        assert!(!dir.path().join("effect").exists());
        assert_eq!(engine.failed_side_effect_pending_approval, was_pending);
        assert_eq!(
            engine
                .reapproval_checkpoint
                .as_ref()
                .unwrap()
                .pending()
                .unwrap(),
            was_pending
        );
    }
}

#[tokio::test]
async fn invalid_host_exit_codes_never_clear_durable_reapproval() {
    for exit_code in [
        serde_json::json!(4294967296_i64),
        serde_json::json!(-4294967296_i64),
        serde_json::json!(i64::MAX),
        serde_json::json!(u64::MAX),
        serde_json::json!(0.0),
        serde_json::json!("0"),
        serde_json::json!({}),
    ] {
        let dir = tempfile::tempdir().unwrap();
        let (mut runtime, mut engine) = fixture(
            dir.path(),
            CoreConfig::default(),
            ExecutionProfile::Legacy,
            ToolRegistry::with_defaults_for_test(),
            vec![tool_call(
                "shell",
                serde_json::json!({"command":"printf forbidden > effect"}),
            )],
        );
        runtime.persist(&mut engine).unwrap();
        engine.failed_side_effect_pending_approval = true;
        engine
            .reapproval_checkpoint
            .as_ref()
            .unwrap()
            .set_pending(true)
            .unwrap();
        turn(&mut engine, &response(serde_json::json!({"behavior":"host_executed_shell","result":{"llmContent":"unconfirmed host completion","metadata":{"exit_code":exit_code}}}))).await.unwrap();
        runtime.persist(&mut engine).unwrap();
        assert_durable_pending(&runtime, &engine);
        assert!(engine.messages.iter().any(|message| {
            message.role == "tool" && format!("{:?}", message.content).contains("invalid exit_code")
        }));
        assert!(!dir.path().join("effect").exists());
    }
}

#[tokio::test]
async fn valid_or_missing_host_exit_codes_preserve_reapproval_semantics() {
    for (exit_code, pending) in [
        (None, true),
        (Some(serde_json::Value::Null), true),
        (Some(serde_json::json!(0)), false),
        (Some(serde_json::json!(23)), true),
        (Some(serde_json::json!(i32::MAX)), true),
        (Some(serde_json::json!(i32::MIN)), true),
    ] {
        let dir = tempfile::tempdir().unwrap();
        let (mut runtime, mut engine) = fixture(
            dir.path(),
            CoreConfig::default(),
            ExecutionProfile::Legacy,
            ToolRegistry::with_defaults_for_test(),
            vec![tool_call(
                "shell",
                serde_json::json!({"command":"printf forbidden > effect"}),
            )],
        );
        runtime.persist(&mut engine).unwrap();
        engine.failed_side_effect_pending_approval = true;
        engine
            .reapproval_checkpoint
            .as_ref()
            .unwrap()
            .set_pending(true)
            .unwrap();
        let metadata = exit_code.map(|code| serde_json::json!({"exit_code":code}));
        turn(&mut engine, &response(serde_json::json!({"behavior":"host_executed_shell","result":{"llmContent":"observed host completion","metadata":metadata}}))).await.unwrap();
        runtime.persist(&mut engine).unwrap();
        assert_eq!(engine.failed_side_effect_pending_approval, pending);
        assert_eq!(
            engine
                .reapproval_checkpoint
                .as_ref()
                .unwrap()
                .pending()
                .unwrap(),
            pending
        );
        assert_eq!(
            runtime
                .store
                .as_ref()
                .unwrap()
                .load(&runtime.record.session_id)
                .unwrap()
                .failed_side_effect_pending_approval,
            pending
        );
        assert!(!dir.path().join("effect").exists());
    }
}

#[path = "approval_response_safety_tests.rs"]
mod approval_response_safety_tests;
