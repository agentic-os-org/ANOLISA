//! Fake-Agent coverage for responsive ACP session orchestration.

use std::time::{Duration, Instant};

use super::*;

const FRAME_LIMIT: usize = 16 * 1024;

#[test]
fn default_command_deadline_outlives_adapter_startup() {
    let config = AcpSessionDriverConfig::new(
        RuntimeLaunchSpec::new("/bin/false", "/"),
        AcpV1ClientConfig::new("cosh-ng", "0.15.0", FRAME_LIMIT),
        "/",
    );

    assert_eq!(config.initialize_timeout, Duration::from_secs(60));
    assert_eq!(config.command_timeout, Duration::from_secs(70));
    assert_eq!(config.event_byte_budget, DEFAULT_EVENT_BYTE_BUDGET);
    assert!(config.validate().is_ok());
}

#[test]
fn invalid_deadline_order_is_rejected_before_launch() {
    let mut config = AcpSessionDriverConfig::new(
        RuntimeLaunchSpec::new("/bin/false", "/"),
        AcpV1ClientConfig::new("cosh-ng", "0.15.0", FRAME_LIMIT),
        "/",
    );
    config.command_timeout = config.initialize_timeout;

    assert!(matches!(
        AcpSessionDriver::launch(config),
        Err(AcpSessionDriverError::InvalidDeadlineConfiguration)
    ));

    let mut config = AcpSessionDriverConfig::new(
        RuntimeLaunchSpec::new("/bin/false", "/"),
        AcpV1ClientConfig::new("cosh-ng", "0.15.0", FRAME_LIMIT),
        "/",
    );
    config.shutdown_grace = config.command_timeout - Duration::from_millis(500);
    assert!(matches!(
        AcpSessionDriver::launch(config),
        Err(AcpSessionDriverError::InvalidDeadlineConfiguration)
    ));
}

#[test]
fn dropping_consumed_observation_releases_aggregate_byte_budget() {
    fn chunk(text: &str) -> AcpV1Observation {
        AcpV1Observation::SessionUpdate {
            session_id: "session".to_owned(),
            update: serde_json::json!({
                "sessionUpdate": "agent_message_chunk",
                "content": {"type": "text", "text": text}
            }),
        }
    }

    let first = chunk("same-sized");
    let budget = serialized_observation_bytes(1, &first);
    let (sender, receiver) = mpsc::sync_channel(EVENT_CAPACITY);
    let mut emitter = ObservationEmitter::new(sender, budget);
    let (_cancel_sender, cancel_receiver) = mpsc::sync_channel(CONTROL_CAPACITY);
    emitter
        .emit(
            first,
            &cancel_receiver,
            Instant::now() + Duration::from_secs(1),
            "test",
        )
        .unwrap();

    let AcpSessionEvent::Observation(first) = receiver.recv().unwrap() else {
        panic!("expected observation")
    };
    assert_eq!(first.sequence, 1);
    let producer = std::thread::spawn(move || {
        emitter.emit(
            chunk("same-sized"),
            &cancel_receiver,
            Instant::now() + Duration::from_secs(1),
            "test",
        )
    });
    assert!(matches!(
        receiver.recv_timeout(Duration::from_millis(20)),
        Err(RecvTimeoutError::Timeout)
    ));
    drop(first);

    let AcpSessionEvent::Observation(second) = receiver.recv().unwrap() else {
        panic!("expected observation")
    };
    assert_eq!(second.sequence, 2);
    producer.join().unwrap().unwrap();
}

#[test]
fn observation_backpressure_is_cancellation_responsive() {
    fn chunk(text: &str) -> AcpV1Observation {
        AcpV1Observation::SessionUpdate {
            session_id: "session".to_owned(),
            update: serde_json::json!({
                "sessionUpdate": "agent_message_chunk",
                "content": {"type": "text", "text": text}
            }),
        }
    }

    let (sender, _receiver) = mpsc::sync_channel(1);
    let mut emitter = ObservationEmitter::new(sender, DEFAULT_EVENT_BYTE_BUDGET);
    let (cancel_sender, cancel_receiver) = mpsc::sync_channel(CONTROL_CAPACITY);
    let deadline = Instant::now() + Duration::from_secs(1);
    emitter
        .emit(chunk("first"), &cancel_receiver, deadline, "prompt")
        .unwrap();
    let producer = std::thread::spawn(move || {
        emitter.emit(chunk("second"), &cancel_receiver, deadline, "prompt")
    });

    std::thread::sleep(Duration::from_millis(20));
    cancel_sender.send(()).unwrap();
    assert!(matches!(
        producer.join().unwrap(),
        Err(AcpSessionDriverError::Cancelled)
    ));
}

#[test]
fn observation_backpressure_preserves_operation_deadline() {
    fn chunk(text: &str) -> AcpV1Observation {
        AcpV1Observation::SessionUpdate {
            session_id: "session".to_owned(),
            update: serde_json::json!({
                "sessionUpdate": "agent_message_chunk",
                "content": {"type": "text", "text": text}
            }),
        }
    }

    let (sender, _receiver) = mpsc::sync_channel(1);
    let mut emitter = ObservationEmitter::new(sender, DEFAULT_EVENT_BYTE_BUDGET);
    let (_cancel_sender, cancel_receiver) = mpsc::sync_channel(CONTROL_CAPACITY);
    emitter
        .emit(
            chunk("first"),
            &cancel_receiver,
            Instant::now() + Duration::from_secs(1),
            "prompt",
        )
        .unwrap();

    assert!(matches!(
        emitter.emit(
            chunk("second"),
            &cancel_receiver,
            Instant::now() + Duration::from_millis(20),
            "prompt",
        ),
        Err(AcpSessionDriverError::Deadline {
            operation: "prompt"
        })
    ));
}

#[cfg(unix)]
fn driver(script: &str, workspace: &tempfile::TempDir) -> AcpSessionDriver {
    driver_with_event_byte_budget(script, workspace, DEFAULT_EVENT_BYTE_BUDGET)
}

#[cfg(unix)]
fn driver_with_event_byte_budget(
    script: &str,
    workspace: &tempfile::TempDir,
    event_byte_budget: usize,
) -> AcpSessionDriver {
    let mut launch = RuntimeLaunchSpec::new("/bin/sh", workspace.path());
    launch.arguments = vec!["-c".into(), script.into()];
    launch.stdin_write_timeout = Duration::from_millis(100);
    let mut config = AcpSessionDriverConfig::new(
        launch,
        AcpV1ClientConfig::new("cosh-ng", "0.15.0", FRAME_LIMIT),
        workspace.path(),
    );
    config.initialize_timeout = Duration::from_secs(2);
    config.prompt_timeout = Duration::from_secs(2);
    config.shutdown_grace = Duration::from_millis(50);
    config.command_timeout = Duration::from_secs(3);
    config.event_byte_budget = event_byte_budget;
    AcpSessionDriver::launch(config).unwrap()
}

#[cfg(unix)]
#[test]
fn default_startup_deadline_accepts_session_after_ten_seconds() {
    let workspace = tempfile::tempdir().unwrap();
    let script = r#"
step=0
while IFS= read -r line; do
    step=$((step + 1))
    case "$step" in
        1) printf '%s\n' '{"jsonrpc":"2.0","id":"cosh-acp-1","result":{"protocolVersion":1,"agentCapabilities":{}}}' ;;
        2)
           sleep 11
           printf '%s\n' '{"jsonrpc":"2.0","id":"cosh-acp-2","result":{"sessionId":"session-1"}}'
           ;;
    esac
done
"#;
    let mut launch = RuntimeLaunchSpec::new("/bin/sh", workspace.path());
    launch.arguments = vec!["-c".into(), script.into()];
    let config = AcpSessionDriverConfig::new(
        launch,
        AcpV1ClientConfig::new("cosh-ng", "0.15.0", FRAME_LIMIT),
        workspace.path(),
    );
    let driver = AcpSessionDriver::launch(config).unwrap();

    driver.initialize().unwrap();
    observation(&driver);
    let started = Instant::now();
    driver.open_session().unwrap();
    assert!(started.elapsed() >= Duration::from_secs(10));
    assert!(matches!(
        observation(&driver),
        AcpV1Observation::SessionOpened { .. }
    ));
    driver.shutdown().unwrap();
}

fn observation(driver: &AcpSessionDriver) -> AcpV1Observation {
    session_observation(driver).observation
}

fn session_observation(driver: &AcpSessionDriver) -> AcpSessionObservation {
    match driver.receive_timeout(Duration::from_secs(2)).unwrap() {
        AcpSessionEvent::Observation(observation) => observation,
        AcpSessionEvent::Terminal(terminal) => {
            panic!(
                "unexpected terminal: {:?} {:?}",
                terminal.kind, terminal.detail
            )
        }
    }
}

#[cfg(unix)]
#[test]
fn driver_streams_one_prompt_and_settles_once() {
    let workspace = tempfile::tempdir().unwrap();
    let script = r#"
step=0
while IFS= read -r line; do
    step=$((step + 1))
    case "$step" in
        1) printf '%s\n' '{"jsonrpc":"2.0","id":"cosh-acp-1","result":{"protocolVersion":1,"agentCapabilities":{}}}' ;;
        2) printf '%s\n' '{"jsonrpc":"2.0","id":"cosh-acp-2","result":{"sessionId":"session-1"}}' ;;
        3)
           printf '%s\n' '{"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"session-1","update":{"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":"hello"}}}}'
           printf '%s\n' '{"jsonrpc":"2.0","id":"cosh-acp-3","result":{"stopReason":"end_turn"}}'
           ;;
    esac
done
"#;
    let driver = driver(script, &workspace);

    driver.initialize().unwrap();
    let initialized = session_observation(&driver);
    assert_eq!(initialized.sequence, 1);
    assert!(matches!(
        initialized.observation,
        AcpV1Observation::Initialized { .. }
    ));
    driver.open_session().unwrap();
    let opened = session_observation(&driver);
    assert_eq!(opened.sequence, 2);
    assert!(matches!(
        opened.observation,
        AcpV1Observation::SessionOpened { .. }
    ));
    driver.prompt("hello").unwrap();
    let chunk = session_observation(&driver);
    assert_eq!(chunk.sequence, 3);
    assert!(matches!(
        chunk.observation,
        AcpV1Observation::SessionUpdate { .. }
    ));
    let finished = session_observation(&driver);
    assert_eq!(finished.sequence, 4);
    assert!(matches!(
        finished.observation,
        AcpV1Observation::PromptFinished { .. }
    ));
    driver.shutdown().unwrap();
    let AcpSessionEvent::Terminal(terminal) =
        driver.receive_timeout(Duration::from_secs(2)).unwrap()
    else {
        panic!("expected terminal")
    };
    assert_eq!(terminal.kind, AcpSessionTerminalKind::Shutdown);
    assert!(terminal.process.is_some());
    assert!(matches!(
        driver.receive_timeout(Duration::from_millis(20)),
        Err(RecvTimeoutError::Disconnected | RecvTimeoutError::Timeout)
    ));
}

#[cfg(unix)]
#[test]
fn eof_during_prompt_is_a_failed_terminal_not_completion() {
    let workspace = tempfile::tempdir().unwrap();
    let script = r#"
step=0
while IFS= read -r line; do
    step=$((step + 1))
    case "$step" in
        1) printf '%s\n' '{"jsonrpc":"2.0","id":"cosh-acp-1","result":{"protocolVersion":1,"agentCapabilities":{}}}' ;;
        2) printf '%s\n' '{"jsonrpc":"2.0","id":"cosh-acp-2","result":{"sessionId":"session-1"}}' ;;
        3) exit 0 ;;
    esac
done
"#;
    let driver = driver(script, &workspace);

    driver.initialize().unwrap();
    observation(&driver);
    driver.open_session().unwrap();
    observation(&driver);
    driver.prompt("disconnect").unwrap();
    let terminal = loop {
        match driver.receive_timeout(Duration::from_secs(2)).unwrap() {
            AcpSessionEvent::Terminal(terminal) => break terminal,
            AcpSessionEvent::Observation(observation) => assert!(
                !matches!(
                    observation.observation,
                    AcpV1Observation::PromptFinished { .. }
                ),
                "EOF cannot complete the prompt"
            ),
        }
    };
    assert_eq!(terminal.kind, AcpSessionTerminalKind::Failed);
}

#[cfg(unix)]
#[test]
fn independent_cancel_reaps_silent_agent() {
    let workspace = tempfile::tempdir().unwrap();
    let script = r#"
step=0
while IFS= read -r line; do
    step=$((step + 1))
    case "$step" in
        1) printf '%s\n' '{"jsonrpc":"2.0","id":"cosh-acp-1","result":{"protocolVersion":1,"agentCapabilities":{}}}' ;;
        2) printf '%s\n' '{"jsonrpc":"2.0","id":"cosh-acp-2","result":{"sessionId":"session-1"}}' ;;
        3) while :; do sleep 1; done ;;
    esac
done
"#;
    let driver = driver(script, &workspace);
    driver.initialize().unwrap();
    observation(&driver);
    driver.open_session().unwrap();
    observation(&driver);
    driver.prompt("wait").unwrap();

    let started = Instant::now();
    driver.control().cancel().unwrap();
    let AcpSessionEvent::Terminal(terminal) =
        driver.receive_timeout(Duration::from_secs(2)).unwrap()
    else {
        panic!("expected terminal")
    };
    assert_eq!(terminal.kind, AcpSessionTerminalKind::Cancelled);
    assert!(started.elapsed() < Duration::from_secs(1));
    assert!(terminal.process.is_some());
}

#[cfg(unix)]
#[test]
fn silent_prompt_timeout_fails_and_reaps_once() {
    let workspace = tempfile::tempdir().unwrap();
    let script = r#"
step=0
while IFS= read -r line; do
    step=$((step + 1))
    case "$step" in
        1) printf '%s\n' '{"jsonrpc":"2.0","id":"cosh-acp-1","result":{"protocolVersion":1,"agentCapabilities":{}}}' ;;
        2) printf '%s\n' '{"jsonrpc":"2.0","id":"cosh-acp-2","result":{"sessionId":"session-1"}}' ;;
        3) while :; do sleep 1; done ;;
    esac
done
"#;
    let mut launch = RuntimeLaunchSpec::new("/bin/sh", workspace.path());
    launch.arguments = vec!["-c".into(), script.into()];
    launch.stdin_write_timeout = Duration::from_millis(100);
    let mut config = AcpSessionDriverConfig::new(
        launch,
        AcpV1ClientConfig::new("cosh-ng", "0.15.0", FRAME_LIMIT),
        workspace.path(),
    );
    config.initialize_timeout = Duration::from_secs(2);
    config.prompt_timeout = Duration::from_millis(50);
    config.shutdown_grace = Duration::from_millis(50);
    config.command_timeout = Duration::from_secs(3);
    let driver = AcpSessionDriver::launch(config).unwrap();
    driver.initialize().unwrap();
    observation(&driver);
    driver.open_session().unwrap();
    observation(&driver);
    driver.prompt("wait").unwrap();

    let AcpSessionEvent::Terminal(terminal) =
        driver.receive_timeout(Duration::from_secs(2)).unwrap()
    else {
        panic!("expected terminal")
    };
    assert_eq!(terminal.kind, AcpSessionTerminalKind::Failed);
    assert!(terminal
        .detail
        .as_deref()
        .is_some_and(|detail| detail.contains("prompt") && detail.contains("deadline")));
    assert!(terminal.process.is_some());
    assert!(driver.receive_timeout(Duration::from_millis(20)).is_err());
}

#[cfg(unix)]
#[test]
fn cancel_settles_pending_permission_before_reap() {
    let workspace = tempfile::tempdir().unwrap();
    let script = r#"
step=0
while IFS= read -r line; do
    step=$((step + 1))
    case "$step" in
        1) printf '%s\n' '{"jsonrpc":"2.0","id":"cosh-acp-1","result":{"protocolVersion":1,"agentCapabilities":{}}}' ;;
        2) printf '%s\n' '{"jsonrpc":"2.0","id":"cosh-acp-2","result":{"sessionId":"session-1"}}' ;;
        3) printf '%s\n' '{"jsonrpc":"2.0","id":41,"method":"session/request_permission","params":{"sessionId":"session-1","toolCall":{"toolCallId":"tool-1","title":"Run"},"options":[{"optionId":"allow","name":"Allow","kind":"allow_once"}]}}' ;;
    esac
done
"#;
    let driver = driver(script, &workspace);
    driver.initialize().unwrap();
    observation(&driver);
    driver.open_session().unwrap();
    observation(&driver);
    driver.prompt("permission").unwrap();
    let AcpV1Observation::PermissionRequested(request) = observation(&driver) else {
        panic!("expected permission request")
    };
    assert_eq!(request.request_id, AcpV1RequestId::Number(41));

    driver.control().cancel().unwrap();
    let AcpSessionEvent::Terminal(terminal) =
        driver.receive_timeout(Duration::from_secs(2)).unwrap()
    else {
        panic!("expected terminal")
    };
    assert_eq!(terminal.kind, AcpSessionTerminalKind::Cancelled);
    assert!(
        terminal.detail.is_none(),
        "cancel frames should encode cleanly"
    );
}

#[cfg(unix)]
#[test]
fn unsupported_callback_is_rejected_by_the_actor() {
    let workspace = tempfile::tempdir().unwrap();
    let script = r#"
step=0
while IFS= read -r line; do
    step=$((step + 1))
    case "$step" in
        1) printf '%s\n' '{"jsonrpc":"2.0","id":"cosh-acp-1","result":{"protocolVersion":1,"agentCapabilities":{}}}' ;;
        2) printf '%s\n' '{"jsonrpc":"2.0","id":"cosh-acp-2","result":{"sessionId":"session-1"}}' ;;
        3) printf '%s\n' '{"jsonrpc":"2.0","id":77,"method":"fs/read_text_file","params":{"sessionId":"session-1","path":"/etc/passwd"}}' ;;
        4)
           printf '%s\n' "$line" | grep -q '"code":-32601' || exit 9
           printf '%s\n' '{"jsonrpc":"2.0","id":"cosh-acp-3","result":{"stopReason":"end_turn"}}'
           ;;
    esac
done
"#;
    let driver = driver(script, &workspace);
    driver.initialize().unwrap();
    observation(&driver);
    driver.open_session().unwrap();
    observation(&driver);
    driver.prompt("unsupported").unwrap();
    assert!(matches!(
        observation(&driver),
        AcpV1Observation::UnsupportedClientRequest { .. }
    ));
    assert!(matches!(
        observation(&driver),
        AcpV1Observation::PromptFinished { .. }
    ));
    driver.shutdown().unwrap();
}

#[cfg(unix)]
#[test]
fn malformed_json_during_initialize_fails_and_reaps_once() {
    let workspace = tempfile::tempdir().unwrap();
    let script = r#"
read -r initialize
printf '%s\n' 'not-json'
while :; do sleep 1; done
"#;
    let driver = driver(script, &workspace);

    assert!(matches!(
        driver.initialize(),
        Err(AcpSessionDriverError::Bridge(_))
    ));
    let AcpSessionEvent::Terminal(terminal) =
        driver.receive_timeout(Duration::from_secs(2)).unwrap()
    else {
        panic!("expected terminal")
    };
    assert_eq!(terminal.kind, AcpSessionTerminalKind::Failed);
    assert!(terminal.process.is_some());
    assert!(driver.receive_timeout(Duration::from_millis(20)).is_err());
}

#[cfg(unix)]
#[test]
fn permission_decision_is_single_use() {
    let workspace = tempfile::tempdir().unwrap();
    let script = r#"
step=0
while IFS= read -r line; do
    step=$((step + 1))
    case "$step" in
        1) printf '%s\n' '{"jsonrpc":"2.0","id":"cosh-acp-1","result":{"protocolVersion":1,"agentCapabilities":{}}}' ;;
        2) printf '%s\n' '{"jsonrpc":"2.0","id":"cosh-acp-2","result":{"sessionId":"session-1"}}' ;;
        3) printf '%s\n' '{"jsonrpc":"2.0","id":41,"method":"session/request_permission","params":{"sessionId":"session-1","toolCall":{"toolCallId":"tool-1","title":"Run"},"options":[{"optionId":"allow","name":"Allow","kind":"allow_once"}]}}' ;;
        4) printf '%s\n' '{"jsonrpc":"2.0","id":"cosh-acp-3","result":{"stopReason":"end_turn"}}' ;;
    esac
done
"#;
    let driver = driver(script, &workspace);
    driver.initialize().unwrap();
    observation(&driver);
    driver.open_session().unwrap();
    observation(&driver);
    driver.prompt("permission").unwrap();
    let AcpV1Observation::PermissionRequested(request) = observation(&driver) else {
        panic!("expected permission request")
    };
    driver
        .answer_permission(
            request.request_id.clone(),
            AcpV1PermissionDecision::Selected {
                option_id: "allow".to_owned(),
            },
        )
        .unwrap();
    assert!(driver
        .answer_permission(request.request_id, AcpV1PermissionDecision::Cancelled)
        .is_err());
    driver.shutdown().unwrap();
}

#[cfg(unix)]
#[test]
fn terminal_is_delivered_after_buffered_observations() {
    let workspace = tempfile::tempdir().unwrap();
    let script = r#"
step=0
while IFS= read -r line; do
    step=$((step + 1))
    case "$step" in
        1) printf '%s\n' '{"jsonrpc":"2.0","id":"cosh-acp-1","result":{"protocolVersion":1,"agentCapabilities":{}}}' ;;
        2) printf '%s\n' '{"jsonrpc":"2.0","id":"cosh-acp-2","result":{"sessionId":"session-1"}}' ;;
        3)
           i=0
           while [ "$i" -lt 40 ]; do
             printf '%s\n' '{"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"session-1","update":{"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":"chunk"}}}}'
             i=$((i + 1))
           done
           printf '%s\n' 'not-json'
           ;;
    esac
done
"#;
    let driver = driver(script, &workspace);
    driver.initialize().unwrap();
    observation(&driver);
    driver.open_session().unwrap();
    observation(&driver);
    driver.prompt("overflow").unwrap();
    std::thread::sleep(Duration::from_millis(100));

    let mut sequences = Vec::new();
    loop {
        match driver.receive_timeout(Duration::from_secs(2)).unwrap() {
            AcpSessionEvent::Observation(observation) => sequences.push(observation.sequence),
            AcpSessionEvent::Terminal(terminal) => {
                assert_eq!(terminal.kind, AcpSessionTerminalKind::Failed);
                assert!(terminal
                    .detail
                    .as_deref()
                    .is_some_and(|detail| !detail.contains("observation queue")));
                break;
            }
        }
    }
    assert_eq!(sequences, (3..=42).collect::<Vec<_>>());
    assert!(driver.receive_timeout(Duration::from_millis(20)).is_err());
}

#[cfg(unix)]
#[test]
fn serialized_byte_saturation_fails_once_without_emitting_the_overflow() {
    let workspace = tempfile::tempdir().unwrap();
    let script = r#"
step=0
while IFS= read -r line; do
    step=$((step + 1))
    case "$step" in
        1) printf '%s\n' '{"jsonrpc":"2.0","id":"cosh-acp-1","result":{"protocolVersion":1,"agentCapabilities":{}}}' ;;
        2) printf '%s\n' '{"jsonrpc":"2.0","id":"cosh-acp-2","result":{"sessionId":"session-1"}}' ;;
        3)
           payload=$(printf '%02048d' 0 | tr '0' x)
           printf '%s\n' "{\"jsonrpc\":\"2.0\",\"method\":\"session/update\",\"params\":{\"sessionId\":\"session-1\",\"update\":{\"sessionUpdate\":\"agent_message_chunk\",\"content\":{\"type\":\"text\",\"text\":\"$payload\"}}}}"
           ;;
    esac
done
"#;
    let driver = driver_with_event_byte_budget(script, &workspace, 1024);
    driver.initialize().unwrap();
    observation(&driver);
    driver.open_session().unwrap();
    observation(&driver);
    driver.prompt("overflow bytes").unwrap();

    let AcpSessionEvent::Terminal(terminal) =
        driver.receive_timeout(Duration::from_secs(2)).unwrap()
    else {
        panic!("overflowing observation must not be emitted")
    };
    assert_eq!(terminal.kind, AcpSessionTerminalKind::Failed);
    assert!(terminal
        .detail
        .as_deref()
        .is_some_and(|detail| detail.contains("observation queue")));
    assert!(driver.receive_timeout(Duration::from_millis(20)).is_err());
}
