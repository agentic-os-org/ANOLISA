use crate::types::{ShellEvent, ShellEventKind, ShellRoutingMetadata};

pub(crate) fn routing_event(
    session_id: String,
    command_id: Option<String>,
    cwd: Option<String>,
    intent: Option<String>,
    timestamp: u64,
    routing: ShellRoutingMetadata,
) -> ShellEvent {
    ShellEvent {
        kind: ShellEventKind::CommandRoutingObserved,
        session_id,
        command_id,
        command: None,
        cwd,
        end_cwd: None,
        exit_code: None,
        started_at_ms: Some(timestamp),
        ended_at_ms: None,
        duration_ms: None,
        terminal_output_ref: None,
        terminal_output_bytes: None,
        input: None,
        component: intent,
        message: None,
        command_origin: None,
        shell_environment_generation: None,
        audit_identity: None,
        routing: Some(routing),
        capture: None,
    }
}
