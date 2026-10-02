//! Lossless terminal-output cleanup: ANSI colour and style codes.
//!
//! "Lossless" here means information-preserving: nothing task-relevant is
//! removed. SGR sequences (`ESC [ … m`) are the only thing this engine strips,
//! because colour and style are the only part of a terminal capture that is
//! provably non-semantic — the same bytes read identically without them.
//!
//! Everything else a terminal stream can contain is left alone on purpose:
//!
//! - **Carriage-return redraws.** Rendering them means deciding what the
//!   screen held, and the discarded tail is sometimes the message. In
//!   `Downloading crate foo v1.2.3 (42%)\r ok` the overwritten text carries
//!   the crate and version; in `abc\rX` it is garbage. Nothing distinguishes
//!   them mechanically, and this stage writes no stash, so a wrong call here
//!   is unrecoverable.
//! - **Cursor positioning.** `ESC [ 2 C` and friends move where the next
//!   write lands. Consuming them without tracking the column does not merely
//!   keep too much; it emits text that was never on screen.
//! - **Erase controls.** Applying one correctly requires the cursor model
//!   above.
//! - **OSC sequences.** `ESC ] 8` wraps a hyperlink, so removing the sequence
//!   drops the URL while keeping the label.
//! - **Spinner and progress glyphs.** Braille frames are animation residue,
//!   but block elements draw both progress bars and bar charts, and deciding
//!   which is a judgement about usefulness rather than about information.
//!
//! Those are all judgements, and judgements belong in the retrievable-lossy
//! stage, which stashes what it removes and emits a marker for it. Recognized
//! repeated progress belongs to the build-log compressor's recoverable stage.

/// Strip ANSI SGR (colour and style) sequences from a terminal capture.
///
/// Matches `ESC [` followed by parameter bytes and a final `m`. An
/// unterminated or non-SGR sequence is left in place — this function removes
/// only what it positively recognizes.
pub(crate) fn clean_terminal(text: &str) -> String {
    if !text.contains('\x1b') {
        return text.to_string();
    }
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find('\x1b') {
        let Some(sgr_len) = sgr_sequence_len(&rest[start..]) else {
            // Not an SGR sequence: keep the ESC and continue past it.
            out.push_str(&rest[..start + 1]);
            rest = &rest[start + 1..];
            continue;
        };
        out.push_str(&rest[..start]);
        rest = &rest[start + sgr_len..];
    }
    out.push_str(rest);
    out
}

/// Byte length of the SGR sequence at the start of `s`, or `None` when `s`
/// does not begin with one. Parameter bytes are digits and `;`, per ECMA-48
/// SGR; anything else disqualifies the sequence so it stays verbatim.
fn sgr_sequence_len(s: &str) -> Option<usize> {
    let body = s.strip_prefix("\x1b[")?;
    let params = body
        .bytes()
        .take_while(|b| b.is_ascii_digit() || *b == b';')
        .count();
    (body.as_bytes().get(params) == Some(&b'm')).then_some(2 + params + 1)
}

#[cfg(test)]
mod tests {
    use super::*;
    include!("tests/terminal_cleanup_tests.rs");
}
