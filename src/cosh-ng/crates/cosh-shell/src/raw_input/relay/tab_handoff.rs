//! Zsh candidate handoff when Tab and later input share one terminal read.

use std::io;

use super::{
    relay_native_passthrough, relay_passthrough_input_with_policy, send_held_input_events,
    InputRelayContext,
};

pub(super) fn relay_coalesced_zsh_tab(
    bytes: &[u8],
    relay: &mut InputRelayContext<'_>,
    emit_activity: bool,
    pending_shell_submits: usize,
    complete_paste: bool,
) -> io::Result<Option<bool>> {
    if relay.zsh_path_prompt_buffering.is_none()
        || complete_paste
        || relay.line_buffer.saw_paste()
        || !relay.line_buffer.pending_partial_bytes().is_empty()
    {
        return Ok(None);
    }
    let Some(tab) = bytes.iter().position(|byte| *byte == b'\t') else {
        return Ok(None);
    };
    if let Some(submit) = bytes
        .iter()
        .position(|byte| matches!(byte, b'\n' | b'\r'))
        .filter(|submit| *submit < tab)
    {
        let submit_end = submit
            + usize::from(
                bytes.get(submit) == Some(&b'\r') && bytes.get(submit + 1) == Some(&b'\n'),
            )
            + 1;
        let routed = relay_passthrough_input_with_policy(
            &bytes[..submit_end],
            relay,
            emit_activity,
            pending_shell_submits,
        )?;
        if submit_end < bytes.len() {
            if !routed {
                relay_passthrough_input_with_policy(
                    &bytes[submit_end..],
                    relay,
                    emit_activity,
                    pending_shell_submits,
                )?;
            } else {
                // A handled submit cut over to a Cosh owner. Match the relay's
                // ownership-crossing rule instead of reinterpreting suffix
                // bytes through the stale ZLE owner.
                send_held_input_events(&bytes[submit_end..], relay.input_events);
            }
        }
        return Ok(Some(true));
    }
    if tab + 1 == bytes.len() {
        return Ok(None);
    }

    let (through_tab, remainder) = bytes.split_at(tab + 1);
    relay_native_passthrough(through_tab, relay, emit_activity, pending_shell_submits)?;
    if let Some(submit) = remainder
        .iter()
        .position(|byte| matches!(byte, b'\n' | b'\r'))
    {
        if submit > 0 {
            relay_passthrough_input_with_policy(
                &remainder[..submit],
                relay,
                emit_activity,
                pending_shell_submits,
            )?;
        }
        let submit_end = submit
            + usize::from(
                remainder.get(submit) == Some(&b'\r') && remainder.get(submit + 1) == Some(&b'\n'),
            )
            + 1;
        if submit_end < remainder.len() {
            // PTYs do not preserve write boundaries, so the current read
            // cannot safely cancel a buffer that the Tab widget may rewrite.
            // Hold later type-ahead until the next read, then cancel and
            // replay it as the next logical line.
            if let Some(buffering) = relay.zsh_path_prompt_buffering.as_deref_mut() {
                buffering.defer_tab_typeahead(&remainder[submit_end..]);
            }
        }
        return Ok(Some(true));
    }
    relay_passthrough_input_with_policy(remainder, relay, emit_activity, pending_shell_submits)
        .map(Some)
}
