//! Command detail projection for the runtime details surface.

use crate::evidence::output_policy::{output_excerpt_status_for_block, terminal_output_id};
use crate::runtime::prelude::*;

pub(super) fn render_command_details<W: Write>(
    state: &InlineState,
    block: &CommandBlock,
    output: &mut W,
) -> std::io::Result<()> {
    let output_id = block
        .output
        .terminal_output_ref
        .as_ref()
        .map(|_| terminal_output_id(&block.session_id, &block.id))
        .unwrap_or_else(|| "<none>".to_string());
    let output_excerpt_status = output_excerpt_status_for_block(block);
    let audit_ref = state
        .audit
        .as_ref()
        .and_then(|audit| audit.command_audit_ref(&block.id))
        .unwrap_or("<none>");
    let title = match state.language {
        Language::ZhCn => "命令详情",
        Language::EnUs => "Command details",
    };
    let status = match block.status {
        CommandStatus::Completed => "completed",
        CommandStatus::Failed => "failed",
    };
    RatatuiInlineRenderer::for_terminal().write_notice_panel(
        output,
        NoticePanelModel {
            title,
            body: vec![
                format!("command_id: {}", block.id),
                format!("audit_ref: {audit_ref}"),
                format!("session_id: {}", block.session_id),
                format!("command: {}", block.command),
                format!("cwd: {}", block.cwd),
                format!("end_cwd: {}", block.end_cwd),
                format!("status: {status}"),
                format!("exit_code: {}", block.exit_code),
                format!("duration_ms: {}", block.duration_ms),
                format!("output_id: {output_id}"),
                format!("output_bytes: {}", block.output.terminal_output_bytes),
                format!("output_excerpt_status: {output_excerpt_status}"),
                "redaction_status: not_requested".to_string(),
                "excerpt_status: not_requested".to_string(),
            ],
            footer: None,
        },
    )
}
