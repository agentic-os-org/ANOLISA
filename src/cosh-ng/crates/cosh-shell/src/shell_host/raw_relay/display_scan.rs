//! Display-stream classification for candidate echo invalidation.

use unicode_width::UnicodeWidthChar;

/// What a span of display-stream bytes does to the screen.
#[derive(Debug)]
pub(super) struct DisplayScan {
    /// Display columns drawn by printable text.
    columns: usize,
    /// The span only restyles or retitles: no text, no cursor movement, no
    /// erase or clear. Cells already on screen keep their position and text.
    pub(super) inert: bool,
}

impl Default for DisplayScan {
    fn default() -> Self {
        Self {
            columns: 0,
            inert: true,
        }
    }
}

impl DisplayScan {
    /// Adds a run of non-escape bytes: printable text draws cells (measured
    /// per Unicode width, CJK = 2) and any control byte except BEL can move
    /// the cursor, so only a BEL-only run leaves the screen untouched.
    fn push_text(&mut self, text: &[u8]) {
        if text.is_empty() {
            return;
        }
        self.inert &= text.iter().all(|byte| *byte == 0x07);
        self.columns += String::from_utf8_lossy(text)
            .chars()
            .map(|ch| ch.width().unwrap_or(0))
            .sum::<usize>();
    }
}

/// Whether a non-CSI, non-OSC escape sequence leaves every cell and the
/// cursor where they were.
///
/// Saving the cursor (`ESC 7`), the keypad modes (`ESC =` / `ESC >`), the
/// charset designators (`ESC ( ) * + , - . /`), `ESC %` charset selection and
/// `ESC SP` 7/8-bit control selection only change how later bytes are
/// interpreted. Restoring the cursor (`ESC 8`) moves it, the `ESC #` family
/// repaints cells (`ESC # 8` fills the screen), and anything incomplete or
/// unrecognized may do either, so only the known-benign forms count.
fn escape_is_inert(intermediates: &[u8], final_byte: Option<u8>) -> bool {
    matches!(
        (intermediates, final_byte),
        ([], Some(b'7' | b'=' | b'>'))
            | (
                [b'(' | b')' | b'*' | b'+' | b',' | b'-' | b'.' | b'/' | b'%' | b' '],
                Some(_)
            )
    )
}

/// Classifies the escape sequences and text of `bytes`.
///
/// Escape sequences are zero-width. SGR, DECSCUSR (CSI ... SP q), charset
/// and keypad selection, DECSC and OSC (BEL- or ST-terminated) leave the
/// cells themselves alone; every other CSI — including zero-width ones such
/// as `\x1b[H\x1b[2J` and `\r\x1b[K` preceded by their C0 controls — moves
/// the cursor or erases cells, as do DECRC (`ESC 8`) and an escape sequence
/// whose remainder has not arrived yet.
pub(super) fn scan_display(bytes: &[u8]) -> DisplayScan {
    let mut scan = DisplayScan::default();
    let mut text_start = 0;
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] != b'\x1b' {
            index += 1;
            continue;
        }
        scan.push_text(&bytes[text_start..index]);
        match bytes.get(index + 1) {
            Some(b'[') => {
                index += 2;
                let params_start = index;
                while index < bytes.len() && (0x20..=0x3f).contains(&bytes[index]) {
                    index += 1;
                }
                let final_byte = bytes.get(index).copied();
                if final_byte.is_some() {
                    index += 1;
                }
                let trailing_space = index > params_start + usize::from(final_byte.is_some())
                    && bytes[index - 2] == b' ';
                let restyles =
                    final_byte == Some(b'm') || (final_byte == Some(b'q') && trailing_space);
                scan.inert &= restyles;
            }
            Some(b']') => {
                // OSC ends at BEL or ST (`ESC \`); without either it runs to
                // the end of the span, which is still invisible.
                index += 2;
                while index < bytes.len() {
                    if bytes[index] == 0x07 {
                        index += 1;
                        break;
                    }
                    if bytes[index] == 0x1b && bytes.get(index + 1) == Some(&b'\\') {
                        index += 2;
                        break;
                    }
                    index += 1;
                }
            }
            Some(_) => {
                index += 1;
                let intermediates_start = index;
                while index < bytes.len() && (0x20..=0x2f).contains(&bytes[index]) {
                    index += 1;
                }
                let intermediates = &bytes[intermediates_start..index];
                let final_byte = bytes.get(index).copied();
                if final_byte.is_some() {
                    index += 1;
                }
                scan.inert &= escape_is_inert(intermediates, final_byte);
            }
            // A lone trailing ESC opens a sequence whose remainder is in the
            // next read. It may turn out to move the cursor, so it is not
            // inert; the scan must also consume it, or it would revisit the
            // same byte forever and freeze the relay.
            None => {
                index += 1;
                scan.inert = false;
            }
        }
        text_start = index;
    }
    scan.push_text(&bytes[text_start..]);
    scan
}

/// Terminal display columns of candidate echo bytes: ANSI escape sequences
/// are zero-width; other content is measured per Unicode width (CJK = 2).
/// Keeps native erase math correct for multi-byte drafts (#1721 G9).
pub(super) fn candidate_display_columns(bytes: &[u8]) -> usize {
    // Sums real display widths (east-asian wide chars count 2), so the
    // erase loop backspaces once per terminal column. Any terminal-specific
    // wide-char overwrite quirk is covered by the `\x1b[K` clear plus the
    // full-line rewrite that always follow the erase.
    scan_display(bytes).columns
}
