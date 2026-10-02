//! Soft-newline sequence whitelist for the natural-language candidate line.
//!
//! Single source of truth (#1721): every recognizer that needs to know
//! whether bytes start with a soft-newline shortcut must call
//! [`soft_newline_sequence_len`]; no second sequence table may exist.

/// Canonical in-buffer representation of a soft newline. Whitelisted
/// shortcut sequences and bracketed-paste newlines are rewritten to this
/// sequence so they stay soft (never submit) and survive re-normalization
/// when a remainder is pushed again.
pub(super) const CANONICAL_SOFT_NEWLINE: &[u8] = b"\x1b\n";

/// Whitelisted soft-newline shortcut sequences.
///
/// - `ESC CR` / `ESC LF`: Alt+Enter as sent by legacy meta-encoding terminals.
/// - `CSI 13;2u` / `CSI 13;3u`: Shift+Enter / Alt+Enter in CSI-u terminals
///   (kitty keyboard protocol, iTerm2 with CSI-u enabled).
/// - `CSI 27;2;13~` / `CSI 27;3;13~`: Shift+Enter / Alt+Enter with xterm
///   `modifyOtherKeys`.
const SOFT_NEWLINE_SEQUENCES: &[&[u8]] = &[
    b"\x1b\r",
    b"\x1b\n",
    b"\x1b[13;2u",
    b"\x1b[13;3u",
    b"\x1b[27;2;13~",
    b"\x1b[27;3;13~",
];

/// Returns the byte length of the soft-newline sequence at the head of
/// `bytes`, or `None` when the head is not a whitelisted sequence.
pub(super) fn soft_newline_sequence_len(bytes: &[u8]) -> Option<usize> {
    SOFT_NEWLINE_SEQUENCES
        .iter()
        .find(|sequence| bytes.starts_with(sequence))
        .map(|sequence| sequence.len())
}

/// Whether any whitelisted soft-newline sequence occurs in `bytes`. Used by
/// the observe-only passthrough tip (#1721 T-c); never consumes bytes.
pub(super) fn contains_soft_newline_sequence(bytes: &[u8]) -> bool {
    first_soft_newline_position(bytes).is_some()
}

/// Byte offset of the first whitelisted soft-newline sequence in `bytes`,
/// if any (#1932 F6): lets the prompt-line upgrade split the chunk around
/// the shortcut.
pub(super) fn first_soft_newline_position(bytes: &[u8]) -> Option<usize> {
    (0..bytes.len()).find(|&idx| soft_newline_sequence_len(&bytes[idx..]).is_some())
}

/// Removes whitelisted soft-newline sequences from passthrough bytes
/// (#1932): with modifyOtherKeys negotiated the terminal emits them on any
/// line, and bash renders the unknown CSI tail as literal garbage
/// (`;2;13~`). The observe-only tip still fires; best-effort single-chunk
/// scan like the tip itself. Returns `None` when nothing needs stripping.
pub(super) fn strip_soft_newline_sequences(bytes: &[u8]) -> Option<Vec<u8>> {
    if !contains_soft_newline_sequence(bytes) {
        return None;
    }
    let mut stripped = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if let Some(len) = soft_newline_sequence_len(&bytes[index..]) {
            index += len;
            continue;
        }
        stripped.push(bytes[index]);
        index += 1;
    }
    Some(stripped)
}

/// Returns the byte length of the soft-newline sequence terminating `bytes`,
/// or `None`. Lets backspace delete a soft newline as one visible character
/// even when the sequence arrived split across chunks (still raw in-buffer).
pub(super) fn soft_newline_suffix_len(bytes: &[u8]) -> Option<usize> {
    SOFT_NEWLINE_SEQUENCES
        .iter()
        .find(|sequence| bytes.ends_with(sequence))
        .map(|sequence| sequence.len())
}

/// Replaces whitelisted soft-newline sequences with the dim `⏎` display
/// marker (or `^J` when the locale is not UTF-8 capable). Display-only:
/// data events keep real newlines.
pub(super) fn render_soft_newline_markers(bytes: &[u8]) -> Vec<u8> {
    let marker = display_marker();
    let mut rendered = Vec::with_capacity(bytes.len());
    let mut idx = 0;
    while idx < bytes.len() {
        if let Some(len) = soft_newline_sequence_len(&bytes[idx..]) {
            rendered.extend_from_slice(marker);
            idx += len;
            continue;
        }
        rendered.push(bytes[idx]);
        idx += 1;
    }
    rendered
}

fn display_marker() -> &'static [u8] {
    if utf8_locale() {
        b"\x1b[2m\xe2\x8f\x8e\x1b[22m" // dim ⏎
    } else {
        b"\x1b[2m^J\x1b[22m"
    }
}

fn utf8_locale() -> bool {
    ["LC_ALL", "LC_CTYPE", "LANG"]
        .iter()
        .find_map(|key| std::env::var(key).ok().filter(|value| !value.is_empty()))
        .is_none_or(|value| {
            let lowered = value.to_ascii_lowercase();
            lowered.contains("utf-8") || lowered.contains("utf8")
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognizes_every_whitelisted_sequence() {
        for sequence in SOFT_NEWLINE_SEQUENCES {
            assert_eq!(
                soft_newline_sequence_len(sequence),
                Some(sequence.len()),
                "sequence {sequence:?} must be recognized",
            );
        }
    }

    #[test]
    fn recognizes_sequence_with_trailing_bytes() {
        assert_eq!(soft_newline_sequence_len(b"\x1b[13;2u tail"), Some(7));
        assert_eq!(soft_newline_sequence_len(b"\x1b\rmore"), Some(2));
    }

    #[test]
    fn rejects_non_whitelisted_sequences() {
        for candidate in [
            b"\x1b[A".as_slice(),     // arrow key
            b"\x1b[13;5u".as_slice(), // Ctrl+Enter: not whitelisted
            b"\x1b[27;2;14~".as_slice(),
            b"\x1b[3~".as_slice(), // delete
            b"\x1bOA".as_slice(),
            b"\r".as_slice(),
            b"\n".as_slice(),
            b"\x1b".as_slice(),      // bare ESC prefix stays pending elsewhere
            b"\x1b[13;2".as_slice(), // incomplete CSI-u prefix
        ] {
            assert_eq!(
                soft_newline_sequence_len(candidate),
                None,
                "candidate {candidate:?} must not be recognized",
            );
        }
    }

    #[test]
    fn canonical_sequence_is_whitelisted() {
        assert_eq!(
            soft_newline_sequence_len(CANONICAL_SOFT_NEWLINE),
            Some(CANONICAL_SOFT_NEWLINE.len()),
        );
    }
}

/// Converts buffered draft bytes into plain text for the prompt draft card
/// (#1721 D13): whitelist soft-newline sequences become `\n`, everything
/// else decodes as UTF-8 (lossy).
pub(super) fn draft_text_from_bytes(bytes: &[u8]) -> String {
    let mut plain: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if let Some(len) = soft_newline_sequence_len(&bytes[index..]) {
            plain.push(b'\n');
            index += len;
            continue;
        }
        plain.push(bytes[index]);
        index += 1;
    }
    String::from_utf8_lossy(&plain).into_owned()
}

#[cfg(test)]
mod draft_text_tests {
    use super::draft_text_from_bytes;

    #[test]
    fn draft_text_normalizes_every_whitelist_sequence() {
        let mut bytes = "第一段".as_bytes().to_vec();
        bytes.extend_from_slice(b"\x1b\n");
        bytes.extend_from_slice("第二段".as_bytes());
        bytes.extend_from_slice(b"\x1b[13;2u");
        bytes.extend_from_slice("第三段".as_bytes());
        assert_eq!(draft_text_from_bytes(&bytes), "第一段\n第二段\n第三段");
    }
}
