use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use cosh_shell::adapter::{
    AgentRunHandle, AgentRunPoll, ClaudeCodeAdapter, CoshCoreAdapter, QwenCliAdapter,
};
use cosh_shell::types::{AgentEvent, AgentRequest};

pub(crate) fn mock_cli_path(name: &str) -> String {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let fixture_path = manifest_dir
        .join("tests")
        .join("fixtures")
        .join("provider")
        .join(provider_fixture_dir(name))
        .join(name);
    if fixture_path.exists() {
        return fixture_path.to_string_lossy().to_string();
    }
    manifest_dir
        .join("tests")
        .join(name)
        .to_string_lossy()
        .to_string()
}

fn provider_fixture_dir(name: &str) -> &'static str {
    if name.starts_with("mock_qwen_") {
        "qwen"
    } else if name.starts_with("mock_cosh_core_") {
        "cosh_core"
    } else {
        "claude"
    }
}

pub(crate) fn make_request(id: &str) -> AgentRequest {
    use cosh_shell::types::*;
    AgentRequest {
        id: id.to_string(),
        session_id: "test-session".to_string(),
        command_block: CommandBlock {
            id: "test-block".to_string(),
            session_id: "test-session".to_string(),
            command: "echo test".to_string(),
            origin: Default::default(),
            cwd: "/tmp".to_string(),
            end_cwd: "/tmp".to_string(),
            started_at_ms: 0,
            ended_at_ms: 0,
            duration_ms: 0,
            exit_code: 1,
            status: CommandStatus::Failed,
            output: OutputRefs {
                terminal_output_ref: None,
                terminal_output_bytes: 0,
            },
            shell_environment_generation: None,
            audit_identity: None,
        },
        context_blocks: vec![],
        context_hints: vec![],
        user_input: Some("test the file".to_string()),
        findings: vec![],
        mode: AgentMode::RecommendOnly,
        user_confirmed: true,
        hook_finding: None,
        recommended_skill: None,
    }
}

pub(crate) fn collect_events_until(
    handle: &AgentRunHandle,
    timeout: Duration,
    predicate: impl Fn(&AgentEvent) -> bool,
) -> Vec<AgentEvent> {
    let mut events = Vec::new();
    let deadline = std::time::Instant::now() + timeout;
    loop {
        if std::time::Instant::now() > deadline {
            break;
        }
        match handle.poll_event_timeout(Duration::from_millis(100)) {
            Ok(AgentRunPoll::Event(event)) => {
                let done = predicate(&event);
                events.push(event);
                if done {
                    break;
                }
            }
            Ok(AgentRunPoll::Timeout) => continue,
            Ok(AgentRunPoll::Finished) => break,
            Err(_) => break,
        }
    }
    events
}

#[allow(clippy::field_reassign_with_default)]
pub(crate) fn make_adapter(mock_script: &str) -> ClaudeCodeAdapter {
    let mut adapter = ClaudeCodeAdapter::default();
    adapter.program = mock_cli_path(mock_script);
    adapter.model = "mock".to_string();
    adapter.max_budget_usd = "1".to_string();
    adapter.allow_model_call = true;
    adapter
}

pub(crate) fn make_qwen_adapter(mock_script: &str) -> QwenCliAdapter {
    QwenCliAdapter {
        program: mock_cli_path(mock_script),
        allow_model_call: true,
        session_id: Arc::default(),
    }
}

pub(crate) fn make_cosh_core_adapter(mock_script: &str) -> CoshCoreAdapter {
    CoshCoreAdapter::new(mock_cli_path(mock_script), true)
}
