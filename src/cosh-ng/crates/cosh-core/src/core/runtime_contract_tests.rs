//! Executable fixtures for the AgentRuntime runtime contracts (S01–S05, V01–V03).
//!
//! Each fixture pins one observable invariant of the existing execution path so
//! a later increment can be compared against a recorded failure.

use super::*;
use crate::provider::mock::MockProvider;
use crate::provider::{GenerateStream, ToolDeclaration};
use crate::tool::Tool;
use async_trait::async_trait;
use std::sync::atomic::AtomicUsize;
use tokio::io::BufReader;

async fn empty_reader() -> tokio::io::Lines<BufReader<&'static [u8]>> {
    BufReader::new(&b""[..]).lines()
}

struct CountingShell {
    calls: Arc<AtomicUsize>,
}

#[async_trait]
impl Tool for CountingShell {
    fn name(&self) -> &str {
        "shell"
    }

    fn description(&self) -> &str {
        "counting shell"
    }

    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": { "command": { "type": "string" } },
            "required": ["command"]
        })
    }

    fn kind(&self) -> ToolKind {
        ToolKind::ShellExec
    }

    async fn invoke(
        &self,
        _params: serde_json::Value,
        _ctx: &ToolContext,
    ) -> Result<ToolResult, String> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(ToolResult::success("provider-native shell executed"))
    }
}

/// One assistant turn declaring two shell calls, so an interruption on the
/// first call leaves the second one declared but unprocessed.
fn two_call_batch() -> MockProvider {
    MockProvider::new(vec![vec![
        GenerateEvent::ToolCallStart {
            index: 0,
            id: "call-a".to_string(),
            name: "shell".to_string(),
        },
        GenerateEvent::ToolCallDelta {
            index: 0,
            arguments_delta: r#"{"command":"echo a"}"#.to_string(),
        },
        GenerateEvent::ToolCallEnd { index: 0 },
        GenerateEvent::ToolCallStart {
            index: 1,
            id: "call-b".to_string(),
            name: "shell".to_string(),
        },
        GenerateEvent::ToolCallDelta {
            index: 1,
            arguments_delta: r#"{"command":"echo b"}"#.to_string(),
        },
        GenerateEvent::ToolCallEnd { index: 1 },
        GenerateEvent::MessageEnd,
    ]])
}

fn declared_call_ids(core: &CoshCore) -> Vec<String> {
    core.messages
        .iter()
        .filter_map(|message| message.tool_calls.as_ref())
        .flatten()
        .map(|call| call.id.clone())
        .collect()
}

fn result_call_ids(core: &CoshCore) -> Vec<String> {
    core.messages
        .iter()
        .filter(|message| message.role == "tool")
        .filter_map(|message| message.tool_call_id.clone())
        .collect()
}

/// S01: an interrupted batch must still pair every declared call with a history
/// result, and must not report the turn as a success.
#[tokio::test]
async fn s01_interrupted_batch_pairs_every_declared_call() {
    let calls = Arc::new(AtomicUsize::new(0));
    let mut config = CoreConfig::default();
    config.agent.approval_mode = ApprovalMode::Recommend;
    let mut tools = ToolRegistry::new();
    tools.register(Box::new(CountingShell {
        calls: Arc::clone(&calls),
    }));
    let mut core = CoshCore::new_legacy(config, Box::new(two_call_batch()), tools);
    core.audit = CoreAuditRecorder::test_capture(&core.session_id);

    let mut reader = empty_reader().await;
    let mut output = Vec::new();
    core.handle_user_message("run both", &mut reader, &mut output)
        .await
        .expect("an unanswered approval ends the turn without a transport error");

    let declared = declared_call_ids(&core);
    let paired = result_call_ids(&core);
    assert_eq!(declared.len(), 2, "the batch declared two calls");
    for id in &declared {
        assert!(
            paired.contains(id),
            "declared call {id} has no paired history result; paired={paired:?}"
        );
    }
    assert_eq!(
        calls.load(Ordering::SeqCst),
        0,
        "no approval arrived, so neither call may run"
    );

    let turn_terminals = core
        .audit
        .captured_events()
        .iter()
        .filter(|event| matches!(event.event_type.as_str(), "turn.completed" | "turn.failed"))
        .map(|event| event.outcome.status.clone())
        .collect::<Vec<_>>();
    assert_eq!(
        turn_terminals,
        vec![AuditOutcomeStatus::Cancelled],
        "exactly one turn terminal, cancelled rather than successful"
    );
}

/// S01b: the turn outcome handed to the transport must distinguish an
/// interruption, so a non-interactive consumer cannot read a cancel as a
/// successful completion.
#[tokio::test]
async fn s01_interrupted_turn_is_not_reported_as_completed() {
    let calls = Arc::new(AtomicUsize::new(0));
    let mut config = CoreConfig::default();
    config.agent.approval_mode = ApprovalMode::Recommend;
    let mut tools = ToolRegistry::new();
    tools.register(Box::new(CountingShell {
        calls: Arc::clone(&calls),
    }));
    let mut core = CoshCore::new_legacy(config, Box::new(two_call_batch()), tools);

    let mut reader = empty_reader().await;
    let mut output = Vec::new();
    let outcome = core
        .handle_user_message("run both", &mut reader, &mut output)
        .await
        .expect("an unanswered approval ends the turn without a transport error");
    assert!(
        !matches!(outcome, AgentTurnOutcome::Completed),
        "an interrupted turn must not be reported as completed"
    );
}

/// Counts loop warnings already projected into the transcript.
fn loop_warnings(core: &CoshCore) -> usize {
    serde_json::to_string(&core.messages)
        .expect("transcript is serializable")
        .matches(LoopDetector::loop_warning())
        .count()
}

fn shell_call(index: u32, id: &str, command: &str) -> Vec<GenerateEvent> {
    vec![
        GenerateEvent::ToolCallStart {
            index,
            id: id.to_string(),
            name: "shell".to_string(),
        },
        GenerateEvent::ToolCallDelta {
            index,
            arguments_delta: format!(r#"{{"command":"{command}"}}"#),
        },
        GenerateEvent::ToolCallEnd { index },
    ]
}

fn text_turn(text: &str) -> Vec<GenerateEvent> {
    vec![
        GenerateEvent::TextDelta(text.to_string()),
        GenerateEvent::MessageEnd,
    ]
}

/// S05a: loop history must not leak across user requests. The second request
/// repeats an action twice in total, which is below the three-repeat threshold
/// for that request on its own.
#[tokio::test]
async fn s05_loop_history_is_scoped_to_one_request() {
    let calls = Arc::new(AtomicUsize::new(0));
    let mut config = CoreConfig::default();
    config.agent.approval_mode = ApprovalMode::Trust;
    let mut tools = ToolRegistry::new();
    tools.register(Box::new(CountingShell {
        calls: Arc::clone(&calls),
    }));
    let mut first = shell_call(0, "r1-a", "uptime");
    first.extend(shell_call(1, "r1-b", "uptime"));
    first.push(GenerateEvent::MessageEnd);
    let mut second = shell_call(0, "r2-a", "uptime");
    second.push(GenerateEvent::MessageEnd);
    let provider = MockProvider::new(vec![first, text_turn("done"), second, text_turn("done")]);
    let mut core = CoshCore::new_legacy(config, Box::new(provider), tools);

    let mut reader = empty_reader().await;
    let mut output = Vec::new();
    core.handle_user_message("first request", &mut reader, &mut output)
        .await
        .expect("first request completes");
    assert_eq!(loop_warnings(&core), 0, "two repeats are below threshold");

    core.handle_user_message("second request", &mut reader, &mut output)
        .await
        .expect("second request completes");
    assert_eq!(
        calls.load(Ordering::SeqCst),
        3,
        "all three calls ran in trust mode"
    );
    assert_eq!(
        loop_warnings(&core),
        0,
        "a new request must not inherit the previous request's loop history"
    );
}

/// S05b: actions that differ only beyond the fingerprint prefix are distinct
/// actions, not a loop.
#[test]
fn s05_distinct_actions_sharing_a_long_prefix_are_not_a_loop() {
    let mut detector = LoopDetector::new();
    let prefix = "p".repeat(240);
    let mut warned = false;
    for suffix in ["a", "b", "c"] {
        warned |= detector.record_action("shell", &format!("{prefix}{suffix}"));
    }
    assert!(
        !warned,
        "three distinct commands sharing a long prefix are not a repeated action"
    );
}

struct CountingWriteFile {
    calls: Arc<AtomicUsize>,
}

#[async_trait]
impl Tool for CountingWriteFile {
    fn name(&self) -> &str {
        "write_file"
    }

    fn description(&self) -> &str {
        "counting write_file"
    }

    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "path": { "type": "string" },
                "content": { "type": "string" }
            },
            "required": ["path", "content"]
        })
    }

    fn kind(&self) -> ToolKind {
        ToolKind::FileEdit
    }

    async fn invoke(
        &self,
        _params: serde_json::Value,
        _ctx: &ToolContext,
    ) -> Result<ToolResult, String> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(ToolResult::success("write accepted"))
    }
}

/// A PreToolUse hook that allows the call and replaces `content` with the given
/// value, so the final action differs from the one the model requested.
fn patching_allow_hook(content: &str) -> crate::config::HookDefinition {
    crate::config::HookDefinition {
        command: format!(
            "printf '%s' '{{\"decision\":\"allow\",\"hookSpecificOutput\":{{\"tool_input\":{{\"content\":\"{content}\"}}}}}}'"
        ),
        name: Some("patch-allow".to_string()),
        matcher: None,
        timeout: Some(10_000),
        sequential: None,
        fail_open: false,
        require_output_when_env: None,
        env: Default::default(),
    }
}

/// Runs one legacy-Auto `write_file` call whose content an allowing hook
/// replaces. `write_file` is deliberately absent from `allowed_tools`, so only
/// the sensitive-write rule can demand approval.
async fn patched_write_core(patched_content: &str) -> (CoshCore, Arc<AtomicUsize>) {
    let calls = Arc::new(AtomicUsize::new(0));
    let provider = MockProvider::new(vec![
        vec![
            GenerateEvent::ToolCallStart {
                index: 0,
                id: "call-write".to_string(),
                name: "write_file".to_string(),
            },
            GenerateEvent::ToolCallDelta {
                index: 0,
                arguments_delta: r#"{"path":"note.txt","content":"safe=true"}"#.to_string(),
            },
            GenerateEvent::ToolCallEnd { index: 0 },
            GenerateEvent::MessageEnd,
        ],
        text_turn("done"),
    ]);
    let mut config = CoreConfig::default();
    config.agent.approval_mode = ApprovalMode::Auto;
    config.agent.allowed_tools = std::collections::HashSet::from(["shell".to_string()]);
    config.hooks.enabled = true;
    config.hooks.pre_tool_use = vec![patching_allow_hook(patched_content)];
    let mut tools = ToolRegistry::new();
    tools.register(Box::new(CountingWriteFile {
        calls: Arc::clone(&calls),
    }));
    let mut core = CoshCore::new_legacy(config, Box::new(provider), tools);
    core.audit = CoreAuditRecorder::test_capture(&core.session_id);

    let mut reader = empty_reader().await;
    let mut output = Vec::new();
    core.handle_user_message("write the note", &mut reader, &mut output)
        .await
        .expect("the turn ends without a transport error");
    (core, calls)
}

/// S02a: the audit event that starts execution must describe the action that
/// actually runs, not the pre-patch arguments. `tool.requested` still records
/// what the model asked for; the execution record carries the final action.
/// The patched content stays benign so the call still executes.
#[tokio::test]
async fn s02_hook_patch_refreshes_the_audited_input_hash() {
    let (core, calls) = patched_write_core("safe=false").await;
    assert_eq!(
        calls.load(Ordering::SeqCst),
        1,
        "the benign patch still runs"
    );
    let event = core
        .audit
        .captured_events()
        .iter()
        .find(|event| {
            event.event_type.as_str() == "tool.execution.started"
                && event.identity.tool_use_id.as_deref() == Some("call-write")
        })
        .expect("the write call reaches an execution record");
    let serialized = serde_json::to_value(event).expect("audit event serializes");
    let final_params = serde_json::json!({
        "path": "note.txt",
        "content": "safe=false"
    });
    assert_eq!(
        serialized["data"]["input_hash"].as_str(),
        Some(hash_json(&final_params).as_str()),
        "the execution record must hash the patched content"
    );
}

/// S02b: classification must follow the final action. A patched sensitive write
/// may not run on the pre-patch `Allow` without a human decision.
#[tokio::test]
async fn s02_hook_patch_reclassifies_a_sensitive_write() {
    let (_core, calls) = patched_write_core("-----END RSA PRIVATE KEY-----").await;
    let executions = calls.load(Ordering::SeqCst);
    assert!(
        executions == 0,
        "a patched sensitive write must wait for a decision: \
         executions_without_decision={executions}"
    );
}

/// A provider whose stream stays pending until `cancel` is called, so a turn
/// can only end early if the interrupt actually reaches the provider.
struct CancellableProvider {
    cancelled: tokio::sync::watch::Sender<bool>,
    state: tokio::sync::watch::Receiver<bool>,
    cancel_calls: Arc<AtomicUsize>,
}

#[async_trait]
impl ContentGenerator for CancellableProvider {
    async fn generate(
        &self,
        _messages: &[Message],
        _tools: &[ToolDeclaration],
        _config: &GenerateConfig,
    ) -> Result<GenerateStream, String> {
        let state = self.state.clone();
        Ok(Box::pin(futures::stream::unfold(
            (state, false),
            |(mut state, emitted)| async move {
                if emitted {
                    return None;
                }
                if !*state.borrow_and_update() {
                    state.changed().await.ok()?;
                }
                Some((GenerateEvent::Cancelled, (state, true)))
            },
        )))
    }

    fn cancel(&self) {
        self.cancel_calls.fetch_add(1, Ordering::SeqCst);
        let _ = self.cancelled.send(true);
    }
}

/// S03: on the legacy path an interrupt arriving during a model call must end
/// the turn cooperatively instead of waiting for the provider to finish.
#[tokio::test]
async fn s03_legacy_model_wait_honours_interrupt() {
    let (cancelled, state) = tokio::sync::watch::channel(false);
    let cancel_calls = Arc::new(AtomicUsize::new(0));
    let provider = CancellableProvider {
        cancelled,
        state,
        cancel_calls: Arc::clone(&cancel_calls),
    };
    let mut config = CoreConfig::default();
    config.agent.approval_mode = ApprovalMode::Trust;
    let mut core = CoshCore::new_legacy(config, Box::new(provider), ToolRegistry::new());

    let interrupt =
        b"{\"type\":\"control_request\",\"request_id\":\"int-1\",\"request\":{\"subtype\":\"interrupt\"}}\n";
    let mut reader = BufReader::new(&interrupt[..]).lines();
    let mut output = Vec::new();
    let turn = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        core.handle_user_message("a long request", &mut reader, &mut output),
    )
    .await;
    assert!(
        turn.is_ok(),
        "an interrupt during a model call must end the turn without waiting for the provider"
    );
    assert!(
        cancel_calls.load(Ordering::SeqCst) > 0,
        "the interrupt must reach provider.cancel()"
    );
}

/// S03b: input the legacy provider wait reads but does not act on must survive
/// in arrival order for the transport loop, never be dropped.
#[tokio::test]
async fn s03_legacy_provider_wait_keeps_unhandled_input_in_order() {
    let (cancelled, state) = tokio::sync::watch::channel(false);
    let cancel_calls = Arc::new(AtomicUsize::new(0));
    let provider = CancellableProvider {
        cancelled,
        state,
        cancel_calls: Arc::clone(&cancel_calls),
    };
    let mut config = CoreConfig::default();
    config.agent.approval_mode = ApprovalMode::Trust;
    let mut core = CoshCore::new_legacy(config, Box::new(provider), ToolRegistry::new());

    let queued = r#"{"type":"user","message":{"role":"user","content":"queued"}}"#;
    let switch = r#"{"type":"control_request","request_id":"sw-1","request":{"subtype":"switch_model","model":"other"}}"#;
    let interrupt =
        r#"{"type":"control_request","request_id":"int-1","request":{"subtype":"interrupt"}}"#;
    let input = format!("{queued}\n{switch}\n{interrupt}\n");
    let mut reader = BufReader::new(input.as_bytes()).lines();
    let mut output = Vec::new();
    let turn = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        core.handle_user_message("a long request", &mut reader, &mut output),
    )
    .await
    .expect("the interrupt must end the turn without waiting for the provider");
    // Where the interrupt lands decides whether the turn reports cancellation
    // or a cancelled-request error; input preservation must hold either way.
    if let Err(error) = turn {
        assert!(
            error.contains("cancel"),
            "the turn must end because of the interrupt, not another failure: {error}"
        );
    }

    assert_eq!(
        core.drain_pending_input(),
        vec![queued.to_string(), switch.to_string()],
        "unhandled input is handed back verbatim and in order"
    );
    assert!(
        core.drain_pending_input().is_empty(),
        "the buffer is handed over exactly once"
    );
}

/// Runs one Recommend-mode shell call answered by a host-executed result, so
/// the classification and audit can be compared against the exact host facts.
async fn host_executed_core(metadata: serde_json::Value) -> CoshCore {
    let calls = Arc::new(AtomicUsize::new(0));
    let provider = MockProvider::new(vec![
        vec![
            GenerateEvent::ToolCallStart {
                index: 0,
                id: "call-host".to_string(),
                name: "shell".to_string(),
            },
            GenerateEvent::ToolCallDelta {
                index: 0,
                arguments_delta: r#"{"command":"sleep 60"}"#.to_string(),
            },
            GenerateEvent::ToolCallEnd { index: 0 },
            GenerateEvent::MessageEnd,
        ],
        text_turn("done"),
    ]);
    let mut config = CoreConfig::default();
    config.agent.approval_mode = ApprovalMode::Recommend;
    let mut tools = ToolRegistry::new();
    tools.register(Box::new(CountingShell {
        calls: Arc::clone(&calls),
    }));
    let mut core = CoshCore::new_legacy(config, Box::new(provider), tools);
    core.audit = CoreAuditRecorder::test_capture(&core.session_id);

    let response = serde_json::json!({
        "type": "control_response",
        "response": {
            "subtype": "success",
            "request_id": "req-0",
            "response": {
                "behavior": "host_executed_shell",
                "result": {
                    "llmContent": "ShellCommandCompleted evidence",
                    "returnDisplay": null,
                    "metadata": metadata,
                }
            }
        }
    });
    let input = format!("{response}\n");
    let mut reader = BufReader::new(input.as_bytes()).lines();
    let mut output = Vec::new();
    core.handle_user_message("run it", &mut reader, &mut output)
        .await
        .expect("a host-executed answer ends the turn without a transport error");
    assert_eq!(
        calls.load(Ordering::SeqCst),
        0,
        "a host-executed result must not run the provider-native executor"
    );
    core
}

fn host_tool_terminal(core: &CoshCore) -> serde_json::Value {
    let event = core
        .audit
        .captured_events()
        .iter()
        .find(|event| {
            matches!(event.event_type.as_str(), "tool.completed" | "tool.failed")
                && event.identity.tool_use_id.as_deref() == Some("call-host")
        })
        .expect("the host-executed call reaches a tool terminal");
    serde_json::to_value(event).expect("audit event serializes")
}

/// S04a: a command the host reports as killed by a signal is not a success,
/// even when no exit code was captured.
#[tokio::test]
async fn s04_host_result_killed_by_signal_is_not_a_success() {
    let core = host_executed_core(serde_json::json!({
        "command": "sleep 60",
        "status": "failed",
        "exit_code": null,
        "signal": "SIGKILL",
    }))
    .await;
    let terminal = host_tool_terminal(&core);
    assert_eq!(
        terminal["event_type"].as_str(),
        Some("tool.failed"),
        "a signalled command must not be recorded as completed: {terminal}"
    );
}

/// S04b: an absent exit code is an unknown outcome, never an implied success.
#[tokio::test]
async fn s04_host_result_without_exit_code_is_not_a_success() {
    let core = host_executed_core(serde_json::json!({
        "command": "sleep 60",
        "status": "completed",
    }))
    .await;
    let terminal = host_tool_terminal(&core);
    assert_ne!(
        terminal["event_type"].as_str(),
        Some("tool.completed"),
        "an unconfirmed exit must not be recorded as completed: {terminal}"
    );
}

/// S04c: the host facts already on the wire reach the audit through the
/// existing tool fields, instead of being dropped at the approval boundary.
#[tokio::test]
async fn s04_host_result_keeps_the_evidence_reference() {
    let core = host_executed_core(serde_json::json!({
        "command": "df -h",
        "status": "completed",
        "exit_code": 0,
        "cwd": "/tmp",
        "end_cwd": "/tmp",
        "duration_ms": 823,
        "output_ref": "terminal-output://block-1",
        "redaction_status": "bounded",
    }))
    .await;
    let terminal = host_tool_terminal(&core);
    assert_eq!(
        terminal["event_type"].as_str(),
        Some("tool.completed"),
        "a confirmed zero exit stays a success: {terminal}"
    );
    assert_eq!(
        terminal["data"]["output_ref"].as_str(),
        Some("terminal-output://block-1"),
        "the evidence reference must survive into the audit: {terminal}"
    );
    assert_eq!(
        terminal["data"]["execution_path"].as_str(),
        Some("host_executed_shell"),
        "the audit must name the host execution path: {terminal}"
    );
}
