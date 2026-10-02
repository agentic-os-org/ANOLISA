use crate::types::{CommandBlock, Finding, GovernedEvent, Intervention};

pub fn render_transcript(
    block: &CommandBlock,
    findings: &[Finding],
    interventions: &[Intervention],
    governed_events: &[GovernedEvent],
) -> Vec<String> {
    let mut lines = vec![
        format!("$ {}", block.command),
        format!("command exited with code {}", block.exit_code),
    ];

    for finding in findings
        .iter()
        .filter(|finding| finding.command_block_id == block.id)
    {
        lines.push(format!("finding: {}", finding.message));
    }

    for intervention in interventions
        .iter()
        .filter(|intervention| intervention.command_block_id == block.id)
    {
        lines.push(format!("suggestion: {}", intervention.guidance));
    }

    if !governed_events.is_empty() {
        lines.push("Agent analysis confirmed".to_string());
    }

    for event in governed_events {
        lines.push(event.display_text.clone());
    }

    if !governed_events.is_empty() {
        lines.push("Display-only: recommendations are not executed automatically".to_string());
    }

    lines
}
