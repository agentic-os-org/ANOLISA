//! Fresh provider-session command without persisted-session mutation.

use crate::adapter::FreshSessionOutcome;
use crate::runtime::prelude::*;
use crate::slash::panel::render_notice_panel;

use super::panel::{render_unavailable, session_management_idle};

pub(super) fn start_fresh_session<W: Write>(
    adapter: &AdapterInstance,
    state: &mut InlineState,
    output: &mut W,
) -> std::io::Result<()> {
    if !session_management_idle(state) {
        return render_notice_panel(
            output,
            state.i18n().t(MessageId::SessionErrorTitle),
            vec![state.i18n().t(MessageId::SessionBusyBody).to_string()],
            None,
        );
    }
    match adapter.start_fresh_session() {
        FreshSessionOutcome::Detached {
            previous_session_id: Some(previous),
        } => render_notice_panel(
            output,
            state.i18n().t(MessageId::SessionNewTitle),
            state
                .i18n()
                .format(MessageId::SessionNewDetachedBody, &[("id", &previous)])
                .lines()
                .map(ToOwned::to_owned)
                .collect(),
            None,
        ),
        FreshSessionOutcome::Detached {
            previous_session_id: None,
        } => render_notice_panel(
            output,
            state.i18n().t(MessageId::SessionNewTitle),
            state
                .i18n()
                .t(MessageId::SessionNewAlreadyFreshBody)
                .lines()
                .map(ToOwned::to_owned)
                .collect(),
            None,
        ),
        // Providers without resumable sessions cannot detach; report the
        // capability limit instead of pretending a fresh session started.
        FreshSessionOutcome::Unsupported => render_unavailable(state, output),
    }
}
