use crate::types::{CommandBlock, ShellEvent, ShellEventKind};

pub(super) fn has_proven_top_level_missing(events: &[ShellEvent], block: &CommandBlock) -> bool {
    events.iter().any(|event| {
        event.kind == ShellEventKind::CommandRoutingObserved
            && event.command_id.as_deref() == Some(block.id.as_str())
            && matches!(event.component.as_deref(), Some("command" | "ambiguous"))
            && event.routing.as_ref().is_some_and(|routing| {
                routing.top_level_missing
                    && routing.proven
                    && !routing.sensitive
                    && !routing.unsafe_input
            })
    })
}
