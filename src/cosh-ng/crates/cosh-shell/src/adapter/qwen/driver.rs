use std::sync::{Arc, Mutex};

use super::super::qwen_stream::QwenStreamParser;
use super::super::{
    control_protocol, start_cancellable_provider_process, start_control_protocol_provider_process,
    AgentRunHandle, ApprovalResponse, PreparedInvocation, ProviderDriverSpec,
    ProviderPromptArgMode, ProviderStreamParser,
};
use crate::types::AgentEvent;

struct QwenDriver;

impl ProviderStreamParser for QwenStreamParser {
    fn parse_line(&mut self, line: &str) -> Vec<AgentEvent> {
        QwenStreamParser::parse_line(self, line)
    }

    fn finish(
        &mut self,
        sink: &mut dyn FnMut(AgentEvent) -> Result<(), super::super::AdapterError>,
    ) -> Result<(), super::super::AdapterError> {
        QwenStreamParser::finish(self, sink)
    }
}

impl ProviderDriverSpec for QwenDriver {
    type Parser = QwenStreamParser;

    const PROVIDER_LABEL: &'static str = "co cli";
    const STREAM_START_MESSAGE: &'static str = "starting co stream-json backend";
    const CONTROL_START_MESSAGE: &'static str = "starting co control protocol backend";
    const PLAIN_PROMPT_MODE: ProviderPromptArgMode = ProviderPromptArgMode::QwenPromptFlag;

    fn parser(run_id: String, pending_session: Arc<Mutex<Option<String>>>) -> Self::Parser {
        QwenStreamParser::new(run_id, Some(pending_session))
    }

    fn map_capabilities(
        mut capabilities: control_protocol::ControlProtocolCapabilities,
    ) -> control_protocol::ControlProtocolCapabilities {
        capabilities.can_handle_host_executed_shell_tool_result = true;
        capabilities
    }

    fn serialize_allow(response: &ApprovalResponse) -> String {
        control_protocol::serialize_co_allow(&response.request_id)
    }
}

pub(super) fn start_cancellable_qwen_process(
    run_id: String,
    prepared: PreparedInvocation,
    session_state: Arc<Mutex<Option<String>>>,
) -> AgentRunHandle {
    start_cancellable_provider_process::<QwenDriver>(run_id, prepared, session_state)
}

pub(super) fn start_control_protocol_qwen_process(
    run_id: String,
    prepared: PreparedInvocation,
    session_state: Arc<Mutex<Option<String>>>,
) -> AgentRunHandle {
    start_control_protocol_provider_process::<QwenDriver>(run_id, prepared, session_state)
}
