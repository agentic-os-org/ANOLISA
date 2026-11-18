use super::panel::acp_adapter;
use crate::adapter::AdapterInstance;
use crate::runtime::prelude::*;
use crate::slash::panel::render_notice_panel;

/// ACP resumes via session/load on the next run; an unknown id fails closed
/// there before any prompt, so selection records the explicit target instead
/// of running the legacy recovery subprocess.
pub(super) fn resume_acp_session<W: Write>(
    adapter: &AdapterInstance,
    session_id: &str,
    state: &InlineState,
    output: &mut W,
) -> Option<std::io::Result<()>> {
    let acp = acp_adapter(adapter)?;
    acp.resume_session(session_id);
    Some(render_notice_panel(
        output,
        state.i18n().t(MessageId::SessionSelectedTitle),
        vec![state
            .i18n()
            .format(MessageId::SessionSelectedBody, &[("id", session_id)])],
        Some(state.i18n().t(MessageId::SessionEvidenceNotRestoredBody)),
    ))
}
