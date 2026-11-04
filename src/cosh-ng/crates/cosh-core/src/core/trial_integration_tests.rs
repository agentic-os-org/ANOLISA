use super::*;
use crate::provider::{DispatchState, ProviderFailure, ProviderFailureKind};
use std::sync::atomic::AtomicUsize;
use tokio::io::AsyncWriteExt;

fn context() -> crate::protocol::EcsTrialContext {
    serde_json::from_value(serde_json::json!({
        "schema_version":1,"shell_session_id":"core-integration",
        "intent":"user_initiated","auto_use_allowed":true
    }))
    .unwrap()
}

#[tokio::test]
async fn provider_rejection_does_not_open_an_answer_or_append_assistant_history() {
    let mut config = CoreConfig::default();
    config.agent.approval_mode = ApprovalMode::Trust;
    let provider =
        crate::provider::mock::MockProvider::new(vec![vec![GenerateEvent::ProviderFailure(
            ProviderFailure {
                code: "Sysom.CreditsExhausted".into(),
                kind: ProviderFailureKind::QuotaExhausted,
                dispatch: DispatchState::NotDispatched,
                request_id: None,
            },
        )]]);
    let mut core = CoshCore::new_legacy(config, Box::new(provider), ToolRegistry::new());
    let mut reader = tokio::io::BufReader::new(&b""[..]).lines();
    let mut output = Vec::new();
    let error = core
        .handle_user_message("explain load", &mut reader, &mut output)
        .await
        .unwrap_err();
    assert_eq!(error, "provider_access:Sysom.CreditsExhausted");
    assert_eq!(core.messages.iter().filter(|m| m.role == "user").count(), 1);
    assert!(!core.messages.iter().any(|m| m.role == "assistant"));
    let events = String::from_utf8(output).unwrap();
    assert!(!events.contains("\"type\":\"message_start\""));
}

struct SummaryRejection {
    calls: Arc<AtomicUsize>,
}

#[async_trait::async_trait]
impl ContentGenerator for SummaryRejection {
    async fn generate(
        &self,
        messages: &[Message],
        _: &[crate::provider::ToolDeclaration],
        _: &GenerateConfig,
    ) -> Result<crate::provider::GenerateStream, String> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        assert!(messages[0]
            .content
            .as_text()
            .contains("session-context compactor"));
        Ok(Box::pin(futures::stream::iter(vec![
            GenerateEvent::ProviderFailure(ProviderFailure {
                code: "Sysom.CreditsExhausted".into(),
                kind: ProviderFailureKind::QuotaExhausted,
                dispatch: DispatchState::NotDispatched,
                request_id: None,
            }),
        ])))
    }
    fn cancel(&self) {}
}

#[tokio::test]
async fn summary_access_failure_stops_before_the_main_question_and_preserves_projection() {
    let mut config = CoreConfig::default();
    config.agent.approval_mode = ApprovalMode::Trust;
    config.session.compaction.model_context_window = Some(40_000);
    let calls = Arc::new(AtomicUsize::new(0));
    let inner: Arc<dyn ContentGenerator> = Arc::new(SummaryRejection {
        calls: calls.clone(),
    });
    let (provider, access) = provider_access::wrap_with_check(inner, context(), Arc::new(|| true));
    let mut core = CoshCore::new_legacy(config, provider, ToolRegistry::new());
    core.trial_access = Some(access.clone());
    for label in ["first", "second", "third"] {
        core.messages
            .push(Message::user(&format!("{label} {}", "x".repeat(28_000))));
        core.messages.push(Message::assistant(&"x".repeat(28_000)));
    }
    let before = core.messages.clone();
    let (_client, input) = tokio::io::duplex(1024);
    let mut reader = tokio::io::BufReader::new(input).lines();
    let mut output = Vec::new();
    let error = core
        .handle_user_message("new question", &mut reader, &mut output)
        .await
        .unwrap_err();
    assert_eq!(error, "provider_access:Sysom.CreditsExhausted");
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert_eq!(
        serde_json::to_value(&core.messages[..before.len()]).unwrap(),
        serde_json::to_value(before).unwrap()
    );
    assert!(core.compaction.state().is_none());
    assert_eq!(access.failure().unwrap().code, "Sysom.CreditsExhausted");
}

struct QueuedTool {
    calls: Arc<AtomicUsize>,
}

#[async_trait::async_trait]
impl crate::tool::Tool for QueuedTool {
    fn name(&self) -> &str {
        "queued_tool"
    }
    fn description(&self) -> &str {
        "count dispatches"
    }
    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({"type":"object"})
    }
    fn kind(&self) -> ToolKind {
        ToolKind::ReadOnly
    }
    async fn invoke(&self, _: serde_json::Value, _: &ToolContext) -> Result<ToolResult, String> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(ToolResult::success("ran"))
    }
}

struct EndWithInterrupt {
    input: Arc<tokio::sync::Mutex<tokio::io::DuplexStream>>,
}

#[async_trait::async_trait]
impl ContentGenerator for EndWithInterrupt {
    async fn generate(
        &self,
        _: &[Message],
        _: &[crate::provider::ToolDeclaration],
        _: &GenerateConfig,
    ) -> Result<crate::provider::GenerateStream, String> {
        let events = vec![
            GenerateEvent::ToolCallStart {
                index: 0,
                id: "tool-one".into(),
                name: "queued_tool".into(),
            },
            GenerateEvent::ToolCallDelta {
                index: 0,
                arguments_delta: "{}".into(),
            },
            GenerateEvent::ToolCallEnd { index: 0 },
            GenerateEvent::MessageEnd,
        ];
        Ok(Box::pin(futures::stream::unfold(
            (events.into_iter(), self.input.clone()),
            |(mut events, input)| async move {
                let event = events.next()?;
                if matches!(event, GenerateEvent::MessageEnd) {
                    input.lock().await.write_all(b"{\"type\":\"control_request\",\"request_id\":\"stop\",\"request\":{\"subtype\":\"interrupt\"}}\n").await.unwrap();
                }
                Some((event, (events, input)))
            },
        )))
    }
    fn cancel(&self) {}
}

#[tokio::test]
async fn interrupt_queued_at_message_end_prevents_the_next_tool_dispatch() {
    let mut config = CoreConfig::default();
    config.agent.approval_mode = ApprovalMode::Trust;
    let (client, input) = tokio::io::duplex(1024);
    let inner: Arc<dyn ContentGenerator> = Arc::new(EndWithInterrupt {
        input: Arc::new(tokio::sync::Mutex::new(client)),
    });
    let (provider, access) = provider_access::wrap_with_check(inner, context(), Arc::new(|| true));
    let calls = Arc::new(AtomicUsize::new(0));
    let mut tools = ToolRegistry::new();
    tools.register(Box::new(QueuedTool {
        calls: calls.clone(),
    }));
    let mut core = CoshCore::new_legacy(config, provider, tools);
    core.trial_access = Some(access);
    let mut reader = tokio::io::BufReader::new(input).lines();
    let mut output = Vec::new();
    tokio::time::timeout(
        Duration::from_secs(2),
        core.handle_user_message("do work", &mut reader, &mut output),
    )
    .await
    .unwrap()
    .unwrap_err();
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}

struct PendingSummary {
    started: Arc<tokio::sync::Notify>,
}

#[async_trait::async_trait]
impl ContentGenerator for PendingSummary {
    async fn generate(
        &self,
        messages: &[Message],
        _: &[crate::provider::ToolDeclaration],
        _: &GenerateConfig,
    ) -> Result<crate::provider::GenerateStream, String> {
        assert!(messages[0]
            .content
            .as_text()
            .contains("session-context compactor"));
        self.started.notify_one();
        std::future::pending().await
    }
    fn cancel(&self) {}
}

#[tokio::test]
async fn cancelling_summary_uses_the_same_reader_without_a_context_limit_error() {
    let mut config = CoreConfig::default();
    config.agent.approval_mode = ApprovalMode::Trust;
    config.session.compaction.model_context_window = Some(40_000);
    let started = Arc::new(tokio::sync::Notify::new());
    let inner: Arc<dyn ContentGenerator> = Arc::new(PendingSummary {
        started: started.clone(),
    });
    let (provider, access) = provider_access::wrap_with_check(inner, context(), Arc::new(|| true));
    let mut core = CoshCore::new_legacy(config, provider, ToolRegistry::new());
    core.trial_access = Some(access);
    for _ in 0..3 {
        core.messages.push(Message::user(&"x".repeat(28_000)));
        core.messages.push(Message::assistant(&"x".repeat(28_000)));
    }
    let (mut client, input) = tokio::io::duplex(1024);
    let mut reader = tokio::io::BufReader::new(input).lines();
    let mut output = Vec::new();
    let controller = async move {
        started.notified().await;
        client.write_all(b"{\"type\":\"control_request\",\"request_id\":\"stop\",\"request\":{\"subtype\":\"interrupt\"}}\n").await.unwrap();
    };
    let (result, ()) = tokio::time::timeout(Duration::from_secs(2), async {
        tokio::join!(
            core.handle_user_message("new question", &mut reader, &mut output),
            controller
        )
    })
    .await
    .unwrap();
    let error = result.unwrap_err();
    assert_eq!(error, "provider_access:trial_cancelled");
    assert!(core.compaction.state().is_none());
    assert!(!String::from_utf8(output).unwrap().contains("context_limit"));
}

struct FirstToolCancels {
    input: Arc<tokio::sync::Mutex<tokio::io::DuplexStream>>,
    calls: Arc<AtomicUsize>,
}

#[async_trait::async_trait]
impl crate::tool::Tool for FirstToolCancels {
    fn name(&self) -> &str {
        "first_tool"
    }
    fn description(&self) -> &str {
        "complete before cancellation"
    }
    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({"type":"object"})
    }
    fn kind(&self) -> ToolKind {
        ToolKind::ReadOnly
    }
    async fn invoke(&self, _: serde_json::Value, _: &ToolContext) -> Result<ToolResult, String> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.input.lock().await.write_all(b"{\"type\":\"control_request\",\"request_id\":\"stop\",\"request\":{\"subtype\":\"interrupt\"}}\n").await.unwrap();
        Ok(ToolResult::success("completed first"))
    }
}

#[tokio::test]
async fn cancelling_a_tool_batch_preserves_paired_history_after_session_reload() {
    let mut config = CoreConfig::default();
    config.agent.approval_mode = ApprovalMode::Trust;
    let (client, input) = tokio::io::duplex(1024);
    let events = vec![
        GenerateEvent::ToolCallStart {
            index: 0,
            id: "tool-first".into(),
            name: "first_tool".into(),
        },
        GenerateEvent::ToolCallDelta {
            index: 0,
            arguments_delta: "{}".into(),
        },
        GenerateEvent::ToolCallEnd { index: 0 },
        GenerateEvent::ToolCallStart {
            index: 1,
            id: "tool-second".into(),
            name: "queued_tool".into(),
        },
        GenerateEvent::ToolCallDelta {
            index: 1,
            arguments_delta: "{}".into(),
        },
        GenerateEvent::ToolCallEnd { index: 1 },
        GenerateEvent::MessageEnd,
    ];
    let inner: Arc<dyn ContentGenerator> =
        Arc::new(crate::provider::mock::MockProvider::new(vec![events]));
    let (provider, access) = provider_access::wrap_with_check(inner, context(), Arc::new(|| true));
    let first = Arc::new(AtomicUsize::new(0));
    let second = Arc::new(AtomicUsize::new(0));
    let mut tools = ToolRegistry::new();
    tools.register(Box::new(FirstToolCancels {
        input: Arc::new(tokio::sync::Mutex::new(client)),
        calls: first.clone(),
    }));
    tools.register(Box::new(QueuedTool {
        calls: second.clone(),
    }));
    let mut core = CoshCore::new_legacy(config, provider, tools);
    core.trial_access = Some(access);
    let mut reader = tokio::io::BufReader::new(input).lines();
    let mut output = Vec::new();
    core.handle_user_message("two tasks", &mut reader, &mut output)
        .await
        .unwrap_err();
    assert_eq!(first.load(Ordering::SeqCst), 1);
    assert_eq!(second.load(Ordering::SeqCst), 0);
    let temp = tempfile::tempdir().unwrap();
    let store = crate::session::SessionStore::for_test_workspace(
        temp.path().join("sessions").to_str().unwrap(),
        temp.path(),
    )
    .unwrap();
    let mut session = crate::session::PersistedSession::new(
        crate::session::ProviderSessionId::new(),
        store.workspace_scope().into(),
        "mock".into(),
        core.messages,
    );
    store.persist(&mut session).unwrap();
    let restored = store.load(&session.session_id).unwrap();
    for id in ["tool-first", "tool-second"] {
        assert_eq!(
            restored
                .messages
                .iter()
                .filter(|m| m.tool_call_id.as_deref() == Some(id))
                .count(),
            1,
            "{id} must have exactly one result"
        );
    }
}
