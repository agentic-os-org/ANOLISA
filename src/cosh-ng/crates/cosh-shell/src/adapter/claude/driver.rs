use std::sync::{Arc, Mutex};

use super::super::{
    control_protocol, start_cancellable_provider_process, start_control_protocol_provider_process,
    AgentRunHandle, ApprovalResponse, ClaudeStreamParser, PreparedInvocation, ProviderDriverSpec,
    ProviderPromptArgMode, ProviderStreamParser,
};
use crate::types::AgentEvent;

struct ClaudeDriver;

impl ProviderStreamParser for ClaudeStreamParser {
    fn parse_line(&mut self, line: &str) -> Vec<AgentEvent> {
        ClaudeStreamParser::parse_line(self, line)
    }

    fn finish(
        &mut self,
        sink: &mut dyn FnMut(AgentEvent) -> Result<(), super::super::AdapterError>,
    ) -> Result<(), super::super::AdapterError> {
        ClaudeStreamParser::finish(self, sink)
    }
}

impl ProviderDriverSpec for ClaudeDriver {
    type Parser = ClaudeStreamParser;

    const PROVIDER_LABEL: &'static str = "claude code";
    const STREAM_START_MESSAGE: &'static str = "starting claude-code stream-json backend";
    const CONTROL_START_MESSAGE: &'static str = "starting claude-code control protocol backend";
    const PLAIN_PROMPT_MODE: ProviderPromptArgMode = ProviderPromptArgMode::TrailingArgIfNonEmpty;

    fn parser(run_id: String, pending_session: Arc<Mutex<Option<String>>>) -> Self::Parser {
        ClaudeStreamParser::new(run_id, Some(pending_session))
    }

    fn map_capabilities(
        capabilities: control_protocol::ControlProtocolCapabilities,
    ) -> control_protocol::ControlProtocolCapabilities {
        capabilities
    }

    fn serialize_allow(response: &ApprovalResponse) -> String {
        match response.tool_input.as_ref() {
            Some(tool_input) => {
                control_protocol::serialize_claude_allow(&response.request_id, tool_input)
            }
            None => control_protocol::serialize_deny(
                &response.request_id,
                "Missing provider tool input",
            ),
        }
    }
}

pub(super) fn start_cancellable_claude_process(
    run_id: String,
    prepared: PreparedInvocation,
    session_state: Arc<Mutex<Option<String>>>,
) -> AgentRunHandle {
    start_cancellable_provider_process::<ClaudeDriver>(run_id, prepared, session_state)
}

pub(super) fn start_control_protocol_claude_process(
    run_id: String,
    prepared: PreparedInvocation,
    session_state: Arc<Mutex<Option<String>>>,
) -> AgentRunHandle {
    start_control_protocol_provider_process::<ClaudeDriver>(run_id, prepared, session_state)
}
