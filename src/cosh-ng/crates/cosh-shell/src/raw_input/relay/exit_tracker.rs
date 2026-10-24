//! Explicit `exit`/`logout` detection over the relayed shell byte stream.
//!
//! Owned by the relay: every byte written to the PTY flows through
//! [`ExplicitExitTracker::observe_shell_bytes`] so the spawn loop can tell
//! an explicit user exit apart from an EOF-driven teardown.

use super::super::event_parser::{
    is_partial_paste_delimiter, BRACKETED_PASTE_END, BRACKETED_PASTE_START,
};

#[derive(Debug, Default)]
pub(in super::super) struct ExplicitExitTracker {
    pending_line: Vec<u8>,
    saw_explicit_exit: bool,
    /// Inside a bracketed-paste region: pasted newlines are inserted into
    /// the readline buffer instead of submitting a line, so they must not
    /// terminate an exit-line match.
    in_paste: bool,
    /// Trailing bytes that form a proper prefix of a paste delimiter,
    /// held until the next chunk resolves them.
    pending_paste_delimiter: Vec<u8>,
}

impl ExplicitExitTracker {
    pub(in super::super) fn observe_shell_bytes(&mut self, bytes: &[u8]) {
        if self.saw_explicit_exit {
            return;
        }
        // PTY reads may split `\x1b[200~` / `\x1b[201~` at any byte; a held
        // partial delimiter is joined with the new chunk before scanning.
        let joined;
        let bytes: &[u8] = if self.pending_paste_delimiter.is_empty() {
            bytes
        } else {
            let mut buffer = std::mem::take(&mut self.pending_paste_delimiter);
            buffer.extend_from_slice(bytes);
            joined = buffer;
            &joined
        };
        let mut idx = 0;
        while idx < bytes.len() {
            if bytes[idx..].starts_with(BRACKETED_PASTE_START) {
                self.in_paste = true;
                idx += BRACKETED_PASTE_START.len();
                continue;
            }
            if bytes[idx..].starts_with(BRACKETED_PASTE_END) {
                self.in_paste = false;
                idx += BRACKETED_PASTE_END.len();
                continue;
            }
            if is_partial_paste_delimiter(&bytes[idx..]) {
                self.pending_paste_delimiter = bytes[idx..].to_vec();
                return;
            }
            if !self.in_paste && matches!(bytes[idx], b'\n' | b'\r') {
                // A newline outside every paste region submits the readline
                // line accumulated so far (the terminator itself is trimmed
                // by the matcher).
                let line = std::mem::take(&mut self.pending_line);
                if is_explicit_exit_line(&line) {
                    self.saw_explicit_exit = true;
                    return;
                }
                idx += 1;
                continue;
            }
            // Pasted payload composes into the readline line like any other
            // byte (including pasted newlines, which stay unsubmitted).
            self.pending_line.push(bytes[idx]);
            idx += 1;
        }
        if self.pending_line.len() > 4096 {
            self.pending_line.clear();
        }
    }

    pub(in super::super) fn saw_explicit_exit(&self) -> bool {
        self.saw_explicit_exit
    }
}

fn is_explicit_exit_line(line: &[u8]) -> bool {
    let text = String::from_utf8_lossy(line);
    let trimmed = text.trim();
    trimmed == "exit" || trimmed.starts_with("exit ") || trimmed == "logout"
}
