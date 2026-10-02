//! Bounded replacement of Bash's internal handoff-wrapper echo.
//!
//! The PTY still receives the complete transport wrapper. This filter replaces
//! only a proven Readline echo with the approved command and fails open for
//! every ambiguous or incomplete candidate.

use std::{borrow::Cow, io};

use super::OscParser;

const MAX_PENDING_BYTES: usize = 64 * 1024;
const MAX_PENDING_LINES: usize = 16;

#[derive(Debug)]
pub(super) struct PendingHandoffEcho {
    command: Vec<u8>,
    replacement: Vec<u8>,
    line: Vec<u8>,
    lines: usize,
    bytes_seen: usize,
}

struct HandoffEcho {
    starts_at: usize,
    ends_at: usize,
}

impl PendingHandoffEcho {
    pub(super) fn new(command: &[u8], replacement: &[u8]) -> Self {
        Self {
            command: command.to_vec(),
            replacement: replacement.to_vec(),
            line: Vec::new(),
            lines: 0,
            bytes_seen: 0,
        }
    }

    pub(super) fn filter<'a>(slot: &mut Option<Self>, data: &'a [u8]) -> Cow<'a, [u8]> {
        if slot.is_none() {
            return Cow::Borrowed(data);
        }

        let mut output = Vec::with_capacity(data.len());
        for byte in data.iter().copied() {
            let Some(pending) = slot.as_mut() else {
                output.push(byte);
                continue;
            };

            pending.line.push(byte);
            pending.bytes_seen += 1;
            if pending.bytes_seen >= MAX_PENDING_BYTES {
                output.extend_from_slice(&pending.line);
                *slot = None;
                continue;
            }

            if byte != b'\n' {
                continue;
            }

            if let Some(echo) = pending.classify_handoff_echo() {
                output.extend_from_slice(&pending.line[..echo.starts_at]);
                output.extend_from_slice(&pending.replacement);
                // Terminal state changes after the accepted input belong to
                // the outer terminal. In particular, dropping ?2004l would
                // leave bracketed-paste mode enabled while the command runs.
                output.extend_from_slice(&pending.line[echo.ends_at..]);
                *slot = None;
                continue;
            }

            // Complete unrelated lines can be asynchronous job output. They
            // fail open immediately while the bounded submission window stays
            // armed for the wrapper echo.
            output.extend_from_slice(&pending.line);
            pending.line.clear();
            pending.lines += 1;
            if pending.lines >= MAX_PENDING_LINES {
                *slot = None;
            }
        }
        Cow::Owned(output)
    }

    pub(super) fn flush(slot: &mut Option<Self>) -> Vec<u8> {
        slot.take().map_or_else(Vec::new, |pending| pending.line)
    }

    fn classify_handoff_echo(&self) -> Option<HandoffEcho> {
        let body = self.line_body()?;
        if self.command.is_empty() {
            return None;
        }

        let mut matched = None;
        let mut complete_candidates = 0;
        for starts_at in body
            .iter()
            .enumerate()
            .filter_map(|(idx, byte)| (*byte == self.command[0]).then_some(idx))
        {
            let Some(ends_at) = match_readline_echo(body, starts_at, &self.command) else {
                continue;
            };
            complete_candidates += 1;
            if complete_candidates > 1 {
                return None;
            }
            if !is_complete_csi_suffix(&body[ends_at..]) {
                continue;
            }
            matched = Some(HandoffEcho { starts_at, ends_at });
        }
        matched
    }

    fn line_body(&self) -> Option<&[u8]> {
        let without_lf = self.line.strip_suffix(b"\n")?;
        Some(without_lf.strip_suffix(b"\r").unwrap_or(without_lf))
    }
}

/// Matches Bash 4.4's verified wrap redraw without accepting arbitrary noise.
///
/// At a terminal-width boundary Readline emits CR followed by the byte it just
/// painted, then resumes the command. The duplicate is accepted only when it
/// equals the immediately preceding command byte.
fn match_readline_echo(line: &[u8], starts_at: usize, command: &[u8]) -> Option<usize> {
    let mut line_idx = starts_at;
    let mut command_idx = 0;

    while command_idx < command.len() {
        if line.get(line_idx) == command.get(command_idx) {
            line_idx += 1;
            command_idx += 1;
            continue;
        }
        if command_idx > 0
            && line.get(line_idx) == Some(&b'\r')
            && line.get(line_idx + 1) == command.get(command_idx - 1)
        {
            line_idx += 2;
            continue;
        }
        return None;
    }

    // A wrap can occur immediately after the last command byte too.
    while line.get(line_idx) == Some(&b'\r') && line.get(line_idx + 1) == command.last() {
        line_idx += 2;
    }
    Some(line_idx)
}

fn is_complete_csi_suffix(bytes: &[u8]) -> bool {
    let mut idx = 0;
    while idx < bytes.len() {
        if bytes.get(idx..idx + 2) != Some(b"\x1b[") {
            return false;
        }
        idx += 2;
        while bytes
            .get(idx)
            .is_some_and(|byte| (0x30..=0x3f).contains(byte))
        {
            idx += 1;
        }
        while bytes
            .get(idx)
            .is_some_and(|byte| (0x20..=0x2f).contains(byte))
        {
            idx += 1;
        }
        let Some(final_byte) = bytes.get(idx) else {
            return false;
        };
        if !(0x40..=0x7e).contains(final_byte) {
            return false;
        }
        idx += 1;
    }
    true
}

impl OscParser {
    pub(in super::super) fn arm_pending_handoff_echo(
        &mut self,
        command: &[u8],
        replacement: &[u8],
    ) -> io::Result<()> {
        // A fresh handoff supersedes the previous window, but any candidate
        // bytes already held by that window still belong to the transcript.
        self.flush_pending_handoff_echo()?;
        self.pending_handoff_echo = Some(PendingHandoffEcho::new(command, replacement));
        Ok(())
    }

    pub(in super::super) fn flush_pending_handoff_echo(&mut self) -> io::Result<()> {
        let pending = PendingHandoffEcho::flush(&mut self.pending_handoff_echo);
        if pending.is_empty() {
            return Ok(());
        }
        self.append_passthrough(&pending)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const WRAPPER: &[u8] = b" _COSH_HANDOFF_DEBUG_TRAP=trap; _COSH_HANDOFF_RETURN_TRAP=trap -p ERR; _cosh_prepare_staged_handoff && eval; _COSH_HANDOFF_STATUS=$?; eval _COSH_HANDOFF_RETURN_TRAP ${_COSH_HANDOFF_RETURN_TRAP} ${_COSH_HANDOFF_DEBUG_TRAP}";

    #[test]
    fn replaces_fragmented_exact_echo() {
        let mut pending = Some(PendingHandoffEcho::new(WRAPPER, b"printf visible"));
        assert!(PendingHandoffEcho::filter(&mut pending, &WRAPPER[..41]).is_empty());
        let mut tail = WRAPPER[41..].to_vec();
        tail.extend_from_slice(b"\r\nfollowing");
        assert_eq!(
            PendingHandoffEcho::filter(&mut pending, &tail).as_ref(),
            b"printf visible\r\nfollowing"
        );
        assert!(pending.is_none());
    }

    #[test]
    fn accepts_verified_bash_44_wrap_redraw_pairs() {
        let mut echo = Vec::new();
        let redraw_after = [23, 59, 91, 127, 166, 197];
        for (idx, byte) in WRAPPER.iter().copied().enumerate() {
            echo.push(byte);
            if redraw_after.contains(&idx) {
                echo.push(b'\r');
                echo.push(byte);
            }
        }
        echo.extend_from_slice(b"\r\n");

        let mut pending = Some(PendingHandoffEcho::new(WRAPPER, b"approved"));
        assert_eq!(
            PendingHandoffEcho::filter(&mut pending, &echo).as_ref(),
            b"approved\r\n"
        );
        assert!(pending.is_none());
    }

    #[test]
    fn matches_observed_bash_44_redraw_fragments() {
        let cases: &[(&[u8], &[u8])] = &[
            (
                b"_COSH_HANDOFF_RETURN_TRAP",
                b"_COSH_HANDOFF\rF_RETURN_TRAP",
            ),
            (b"trap -p ERR", b"trap -p \r ERR"),
            (
                b"_cosh_prepare_staged_handoff",
                b"_cosh_prepare_staged_han\rndoff",
            ),
            (b"STATUS=$?; eval", b"STATUS=$?; eva\ral"),
            (
                b"_COSH_HANDOFF_RETURN_TRAP _COSH",
                b"_COSH_HANDOFF_RETURN_TRAP\rP _COSH",
            ),
            (
                b"${_COSH_HANDOFF_RETURN_TRAP}",
                b"${_COSH_HANDOFF_RETURN_TR\rRAP}",
            ),
            (b"_DEBUG_TRAP", b"_DEBU\rUG_TRAP"),
        ];

        for (command, observed) in cases {
            let mut echo = observed.to_vec();
            echo.extend_from_slice(b"\r\n");
            let mut pending = Some(PendingHandoffEcho::new(command, b"approved"));
            assert_eq!(
                PendingHandoffEcho::filter(&mut pending, &echo).as_ref(),
                b"approved\r\n"
            );
            assert!(pending.is_none());
        }
    }

    #[test]
    fn preserves_trailing_bracketed_paste_disable() {
        let mut echo = WRAPPER.to_vec();
        echo.extend_from_slice(b"\x1b[?2004l\r\n");
        let mut pending = Some(PendingHandoffEcho::new(WRAPPER, b"approved"));
        assert_eq!(
            PendingHandoffEcho::filter(&mut pending, &echo).as_ref(),
            b"approved\x1b[?2004l\r\n"
        );
        assert!(pending.is_none());
    }

    #[test]
    fn preserves_fragmented_multiple_csi_and_replacement_text() {
        let replacement = b"printf '\tfirst\nsecond'";
        let mut first = WRAPPER.to_vec();
        first.extend_from_slice(b"\x1b[?20");
        let mut pending = Some(PendingHandoffEcho::new(WRAPPER, replacement));
        assert!(PendingHandoffEcho::filter(&mut pending, &first).is_empty());
        assert_eq!(
            PendingHandoffEcho::filter(&mut pending, b"04l\x1b[0m\r\n").as_ref(),
            b"printf '\tfirst\nsecond'\x1b[?2004l\x1b[0m\r\n"
        );
        assert!(pending.is_none());
    }

    #[test]
    fn wrong_duplicate_and_bare_carriage_return_fail_open() {
        for mutation in [b"\rX".as_slice(), b"\r".as_slice()] {
            let mut echo = WRAPPER[..40].to_vec();
            echo.extend_from_slice(mutation);
            echo.extend_from_slice(&WRAPPER[40..]);
            echo.extend_from_slice(b"\r\n");
            let mut pending = Some(PendingHandoffEcho::new(WRAPPER, b"approved"));
            assert_eq!(
                PendingHandoffEcho::filter(&mut pending, &echo).as_ref(),
                echo
            );
            assert!(pending.is_some());
        }
    }

    #[test]
    fn malformed_or_non_csi_suffix_fails_open() {
        for suffix in [b"\x1b[?2004\r\n".as_slice(), b"unexpected\r\n".as_slice()] {
            let mut echo = WRAPPER.to_vec();
            echo.extend_from_slice(suffix);
            let mut pending = Some(PendingHandoffEcho::new(WRAPPER, b"approved"));
            assert_eq!(
                PendingHandoffEcho::filter(&mut pending, &echo).as_ref(),
                echo
            );
        }
    }

    #[test]
    fn preserves_unrelated_prefix_and_rejects_ambiguous_candidates() {
        let mut echo = b"background".to_vec();
        echo.extend_from_slice(WRAPPER);
        echo.extend_from_slice(b"\r\n");
        let mut pending = Some(PendingHandoffEcho::new(WRAPPER, b"approved"));
        assert_eq!(
            PendingHandoffEcho::filter(&mut pending, &echo).as_ref(),
            b"backgroundapproved\r\n"
        );

        let mut ambiguous = WRAPPER.to_vec();
        ambiguous.extend_from_slice(WRAPPER);
        ambiguous.extend_from_slice(b"\r\n");
        let mut pending = Some(PendingHandoffEcho::new(WRAPPER, b"approved"));
        assert_eq!(
            PendingHandoffEcho::filter(&mut pending, &ambiguous).as_ref(),
            ambiguous
        );
    }

    #[test]
    fn caps_release_exact_bytes_and_flush_releases_partial() {
        let bytes = vec![b'x'; MAX_PENDING_BYTES];
        let mut pending = Some(PendingHandoffEcho::new(WRAPPER, b"approved"));
        assert_eq!(PendingHandoffEcho::filter(&mut pending, &bytes), bytes);
        assert!(pending.is_none());

        let mut pending = Some(PendingHandoffEcho::new(WRAPPER, b"approved"));
        assert!(PendingHandoffEcho::filter(&mut pending, b" partial").is_empty());
        assert_eq!(PendingHandoffEcho::flush(&mut pending), b" partial");
        assert!(pending.is_none());
    }

    #[test]
    fn parser_rearm_and_abort_flush_partial_candidates() {
        let temp = tempfile::tempdir().unwrap();
        let mut parser = OscParser::new(
            "handoff-echo-rearm".to_string(),
            temp.path().join("output-ref"),
            "marker-token".to_string(),
        );

        parser.arm_pending_handoff_echo(WRAPPER, b"first").unwrap();
        parser.append_passthrough(b" partial-first").unwrap();
        assert_eq!(parser.display_position(), 0);

        parser.arm_pending_handoff_echo(WRAPPER, b"second").unwrap();
        assert_eq!(
            parser
                .read_display_range(0, parser.display_position())
                .unwrap()
                .as_ref(),
            b" partial-first"
        );

        parser.append_passthrough(b" partial-second").unwrap();
        parser.flush_pending_handoff_echo().unwrap();
        assert_eq!(
            parser
                .read_display_range(0, parser.display_position())
                .unwrap()
                .as_ref(),
            b" partial-first partial-second"
        );
    }

    #[test]
    fn line_cap_preserves_every_non_matching_line() {
        let mut pending = Some(PendingHandoffEcho::new(WRAPPER, b"approved"));
        for _ in 0..MAX_PENDING_LINES {
            assert_eq!(
                PendingHandoffEcho::filter(&mut pending, b"background\r\n").as_ref(),
                b"background\r\n"
            );
        }
        assert!(pending.is_none());
    }

    #[test]
    fn unarmed_filter_borrows_input() {
        let mut pending = None;
        assert!(matches!(
            PendingHandoffEcho::filter(&mut pending, b"ordinary"),
            Cow::Borrowed(b"ordinary")
        ));
    }
}
