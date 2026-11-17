use crate::runtime::prelude::*;

pub(crate) fn write_shell_prompt<W: Write>(
    state: &mut InlineState,
    _output: &mut W,
) -> std::io::Result<()> {
    // The guide hosts auth as a finite task: a finished action returns to the
    // management list, never to a working shell prompt.
    if state.auth.host.is_guide() {
        state.auth.menu_reload = true;
        return Ok(());
    }
    state.trigger_pty_prompt = true;
    Ok(())
}

pub(super) fn clear_shell_prompt_line<W: Write>(output: &mut W) -> std::io::Result<()> {
    write!(output, "\r\x1b[2K")
}
