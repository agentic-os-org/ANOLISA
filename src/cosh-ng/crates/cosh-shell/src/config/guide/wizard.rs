//! Shared guide wizard content and terminal-capability rules.
//!
//! The interactive controller lives in `runtime::guide` because it reads
//! login and auth owners that only exist in the binary target.

use std::ffi::OsString;
use std::io::IsTerminal;

use crate::ui::guide::{MIN_COLUMNS, MIN_ROWS};

pub(crate) mod content;
#[cfg(test)]
library_tests! {
    mod tests;
}

pub(crate) use content::{plain_text, usage_text};

/// Only the bare `cosh guide` shape opens the wizard; leaves keep their CLI.
pub(crate) fn wants_wizard(args: &[OsString]) -> bool {
    args.is_empty()
}

pub(crate) fn terminal_capable() -> bool {
    let tty = std::io::stdin().is_terminal()
        && std::io::stdout().is_terminal()
        && std::io::stderr().is_terminal();
    tty && terminal_ready(
        std::env::var_os("TERM").as_deref(),
        ratatui::crossterm::terminal::size().ok(),
    )
}

/// VNC, SSH, and serial are not decision inputs: only real terminal capability.
fn terminal_ready(term: Option<&std::ffi::OsStr>, size: Option<(u16, u16)>) -> bool {
    let Some(term) = term.and_then(|value| value.to_str()) else {
        return false;
    };
    if term.is_empty() || term.eq_ignore_ascii_case("dumb") {
        return false;
    }
    let Some((columns, rows)) = size else {
        return false;
    };
    columns >= MIN_COLUMNS && rows >= MIN_ROWS
}
