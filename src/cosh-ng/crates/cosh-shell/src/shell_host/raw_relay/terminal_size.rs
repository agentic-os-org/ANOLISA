//! Outer-terminal size synchronization for the shell PTY.

use std::io;

use nix::libc;
use nix::pty::Winsize;

use crate::raw_input::{set_pty_winsize, signal_process_group};

use super::super::model::current_terminal_winsize;

pub(in crate::shell_host) fn sync_outer_terminal_winsize(
    master_fd: i32,
    child_pid: u32,
    last_winsize: &mut Winsize,
    presentation: &mut super::super::prompt_presentation::PromptPresentation,
) -> io::Result<()> {
    let Some(current) = current_terminal_winsize() else {
        return Ok(());
    };
    if same_winsize(&current, last_winsize) {
        return Ok(());
    }

    set_pty_winsize(master_fd, current)?;
    signal_process_group(child_pid, libc::SIGWINCH)?;
    *last_winsize = current;
    presentation
        .selection_surface_mut()
        .note_resize(current.ws_col, current.ws_row);
    Ok(())
}

fn same_winsize(left: &Winsize, right: &Winsize) -> bool {
    left.ws_row == right.ws_row
        && left.ws_col == right.ws_col
        && left.ws_xpixel == right.ws_xpixel
        && left.ws_ypixel == right.ws_ypixel
}
