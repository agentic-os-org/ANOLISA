//! Explicit `exit`/`logout` detection over the relayed shell byte stream.
//!
//! Owned by the relay: every byte written to the PTY flows through
//! [`ExplicitExitTracker::observe_shell_bytes`] so the spawn loop can tell
//! an explicit user exit apart from an EOF-driven teardown.

#[derive(Debug, Default)]
pub(in super::super) struct ExplicitExitTracker {
    pending_line: Vec<u8>,
    saw_explicit_exit: bool,
}

impl ExplicitExitTracker {
    pub(in super::super) fn observe_shell_bytes(&mut self, bytes: &[u8]) {
        if self.saw_explicit_exit {
            return;
        }
        self.pending_line.extend_from_slice(bytes);
        while let Some(idx) = self
            .pending_line
            .iter()
            .position(|byte| matches!(byte, b'\n' | b'\r'))
        {
            let line = self.pending_line.drain(..=idx).collect::<Vec<_>>();
            if is_explicit_exit_line(&line) {
                self.saw_explicit_exit = true;
                self.pending_line.clear();
                return;
            }
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
