//! Single synchronous byte source shared by the wizard and the finite auth task.
//!
//! The wizard decodes only navigation keys; the auth task consumes the same
//! bytes through the existing card capture parser. Neither path uses the
//! crossterm event reader, whose process-global queue cannot be reset when
//! terminal ownership moves between the two.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

use nix::libc;
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

pub(crate) const POLL_TIMEOUT_MS: i32 = 200;
const ESCAPE_TIMEOUT: Duration = Duration::from_millis(30);
const MAX_SEQUENCE_BYTES: usize = 32;
const PASTE_TAIL_BYTES: usize = 6;
const PASTE_START: &[u8] = b"\x1b[200~";
const PASTE_END: &[u8] = b"\x1b[201~";
const READ_CHUNK: usize = 256;

pub(crate) enum Input {
    Key(KeyEvent),
    Resize,
    Tick,
    /// The terminal input source closed or failed permanently.
    Lost,
}

pub(crate) enum Batch {
    Bytes(Vec<u8>),
    Idle,
    Lost,
}

#[derive(Default)]
pub(crate) struct ByteSource {
    lost: bool,
}

impl ByteSource {
    pub(crate) fn read_batch(&mut self, timeout_ms: i32) -> Batch {
        if self.lost {
            return Batch::Lost;
        }
        let mut descriptor = libc::pollfd {
            fd: libc::STDIN_FILENO,
            events: libc::POLLIN,
            revents: 0,
        };
        let ready = unsafe { libc::poll(&mut descriptor, 1, timeout_ms) };
        if ready < 0 {
            let error = std::io::Error::last_os_error();
            if error.kind() == std::io::ErrorKind::Interrupted {
                return Batch::Idle;
            }
            self.lost = true;
            return Batch::Lost;
        }
        if ready == 0 {
            return Batch::Idle;
        }
        let readable = descriptor.revents & libc::POLLIN != 0;
        let closed = descriptor.revents & (libc::POLLHUP | libc::POLLERR | libc::POLLNVAL) != 0;
        if !readable {
            self.lost = true;
            return Batch::Lost;
        }
        let mut buffer = [0u8; READ_CHUNK];
        let read =
            unsafe { libc::read(libc::STDIN_FILENO, buffer.as_mut_ptr().cast(), buffer.len()) };
        if read < 0 {
            let error = std::io::Error::last_os_error();
            return match error.kind() {
                std::io::ErrorKind::Interrupted | std::io::ErrorKind::WouldBlock => Batch::Idle,
                _ => {
                    self.lost = true;
                    Batch::Lost
                }
            };
        }
        if read == 0 {
            self.lost = true;
            return Batch::Lost;
        }
        // A hangup observed with the final bytes still delivers them once; the
        // next read reports the loss so an in-flight submit can converge first.
        if closed {
            self.lost = true;
        }
        Batch::Bytes(buffer[..read as usize].to_vec())
    }
}

pub(crate) struct GuideInput {
    source: ByteSource,
    decoder: KeyDecoder,
    size: Option<(u16, u16)>,
}

impl GuideInput {
    pub(crate) fn new() -> Self {
        Self {
            source: ByteSource::default(),
            decoder: KeyDecoder::default(),
            size: ratatui::crossterm::terminal::size().ok(),
        }
    }

    pub(crate) fn next(&mut self) -> Input {
        if self.size_changed() {
            return Input::Resize;
        }
        if let Some(key) = self.decoder.pop() {
            return Input::Key(key);
        }
        match self.source.read_batch(POLL_TIMEOUT_MS) {
            Batch::Bytes(bytes) => {
                self.decoder.push(&bytes);
                if self.size_changed() {
                    return Input::Resize;
                }
                self.decoder.pop().map(Input::Key).unwrap_or(Input::Tick)
            }
            Batch::Idle => {
                if let Some(key) = self.decoder.expire() {
                    return Input::Key(key);
                }
                if self.size_changed() {
                    return Input::Resize;
                }
                Input::Tick
            }
            Batch::Lost => Input::Lost,
        }
    }

    /// Reports a terminal geometry change once, so a hosted task can redraw without waiting for a key.
    pub(crate) fn size_changed(&mut self) -> bool {
        let size = ratatui::crossterm::terminal::size().ok();
        if size == self.size {
            return false;
        }
        self.size = size;
        true
    }

    pub(crate) fn read_raw(&mut self) -> Batch {
        self.source.read_batch(POLL_TIMEOUT_MS)
    }

    pub(crate) fn discard_auth_submission_input(&mut self) -> std::io::Result<()> {
        self.discard_pending();
        nix::errno::Errno::result(unsafe { libc::tcflush(libc::STDIN_FILENO, libc::TCIFLUSH) })?;
        Ok(())
    }

    /// Drops bytes this process already read for the previous owner. Terminal
    /// input still queued in the kernel belongs to the caller, not to us.
    pub(crate) fn discard_pending(&mut self) {
        self.decoder.clear();
    }
}

#[derive(Default)]
struct KeyDecoder {
    pending: Vec<u8>,
    since: Option<Instant>,
    ready: VecDeque<KeyEvent>,
    pasting: bool,
}

impl KeyDecoder {
    fn pop(&mut self) -> Option<KeyEvent> {
        self.ready.pop_front()
    }

    fn clear(&mut self) {
        self.pending.clear();
        self.ready.clear();
        self.since = None;
        self.pasting = false;
    }

    fn push(&mut self, bytes: &[u8]) {
        self.pending.extend_from_slice(bytes);
        self.parse();
    }

    fn expire(&mut self) -> Option<KeyEvent> {
        if self.pending.is_empty() {
            return None;
        }
        let elapsed = self
            .since
            .is_some_and(|since| since.elapsed() >= ESCAPE_TIMEOUT);
        if !elapsed {
            return None;
        }
        let lone_escape = !self.pasting && self.pending == [0x1b];
        self.pending.clear();
        self.since = None;
        // An unterminated paste must not swallow the keyboard forever.
        self.pasting = false;
        lone_escape.then(|| KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE))
    }

    fn parse(&mut self) {
        loop {
            if self.pasting {
                if let Some(end) = find(&self.pending, PASTE_END) {
                    self.pending.drain(..end + PASTE_END.len());
                    self.pasting = false;
                    self.since = None;
                    continue;
                }
                let keep = self.pending.len().min(PASTE_TAIL_BYTES);
                let drop = self.pending.len() - keep;
                self.pending.drain(..drop);
                self.since.get_or_insert_with(Instant::now);
                return;
            }
            let Some(&first) = self.pending.first() else {
                self.since = None;
                return;
            };
            if first == 0x1b {
                match self.parse_escape() {
                    Escape::Consumed(count) => {
                        self.pending.drain(..count);
                        self.since = None;
                    }
                    Escape::Paste(count) => {
                        self.pending.drain(..count);
                        self.pasting = true;
                        self.since = Some(Instant::now());
                    }
                    Escape::Incomplete => {
                        if self.pending.len() > MAX_SEQUENCE_BYTES {
                            self.pending.clear();
                            self.since = None;
                            return;
                        }
                        self.since.get_or_insert_with(Instant::now);
                        return;
                    }
                    Escape::Unknown(count) => {
                        self.pending.drain(..count);
                        self.since = None;
                    }
                }
                continue;
            }
            if first >= 0x80 {
                let width = utf8_width(first);
                if self.pending.len() < width {
                    if self.pending.len() > MAX_SEQUENCE_BYTES {
                        self.pending.clear();
                        return;
                    }
                    self.since.get_or_insert_with(Instant::now);
                    return;
                }
                self.pending.drain(..width);
                self.since = None;
                continue;
            }
            self.pending.remove(0);
            self.since = None;
            if let Some(key) = plain_key(first) {
                self.ready.push_back(key);
            }
        }
    }

    fn parse_escape(&mut self) -> Escape {
        if self.pending.starts_with(PASTE_START) {
            return Escape::Paste(PASTE_START.len());
        }
        match self.pending.get(1) {
            None => Escape::Incomplete,
            Some(b'[') => {
                if self.pending.len() == 2 {
                    return Escape::Incomplete;
                }
                let Some(offset) = self.pending[2..]
                    .iter()
                    .position(|byte| (0x40..=0x7e).contains(byte))
                else {
                    return Escape::Incomplete;
                };
                let end = 2 + offset;
                let parameters = &self.pending[2..end];
                if let Some(key) = csi_key(parameters, self.pending[end]) {
                    self.ready.push_back(key);
                }
                Escape::Consumed(end + 1)
            }
            Some(b'O') => match self.pending.get(2) {
                None => Escape::Incomplete,
                Some(byte) => {
                    if let Some(code) = match byte {
                        b'A' => Some(KeyCode::Up),
                        b'B' => Some(KeyCode::Down),
                        b'C' => Some(KeyCode::Right),
                        b'D' => Some(KeyCode::Left),
                        b'Q' => Some(KeyCode::F(2)),
                        _ => None,
                    } {
                        self.ready
                            .push_back(KeyEvent::new(code, KeyModifiers::NONE));
                    }
                    Escape::Consumed(3)
                }
            },
            Some(0x1b) => {
                // A repeated Esc is a real key press, not an unknown sequence.
                self.ready
                    .push_back(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
                Escape::Consumed(1)
            }
            Some(_) => Escape::Unknown(2),
        }
    }
}

enum Escape {
    Consumed(usize),
    Paste(usize),
    Incomplete,
    Unknown(usize),
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

fn utf8_width(lead: u8) -> usize {
    match lead {
        0xc0..=0xdf => 2,
        0xe0..=0xef => 3,
        0xf0..=0xf7 => 4,
        _ => 1,
    }
}

fn plain_key(byte: u8) -> Option<KeyEvent> {
    let (code, modifiers) = match byte {
        b'\r' | b'\n' => (KeyCode::Enter, KeyModifiers::NONE),
        0x03 => (KeyCode::Char('c'), KeyModifiers::CONTROL),
        0x04 => (KeyCode::Char('d'), KeyModifiers::CONTROL),
        0x7f | 0x08 => (KeyCode::Backspace, KeyModifiers::NONE),
        b'\t' => (KeyCode::Tab, KeyModifiers::NONE),
        0x20..=0x7e => (KeyCode::Char(byte as char), KeyModifiers::NONE),
        _ => return None,
    };
    Some(KeyEvent::new(code, modifiers))
}

fn csi_key(parameters: &[u8], final_byte: u8) -> Option<KeyEvent> {
    let code = match (parameters, final_byte) {
        (b"", b'A') => KeyCode::Up,
        (b"", b'B') => KeyCode::Down,
        (b"", b'C') => KeyCode::Right,
        (b"", b'D') => KeyCode::Left,
        (b"5", b'~') => KeyCode::PageUp,
        (b"6", b'~') => KeyCode::PageDown,
        (b"12", b'~') => KeyCode::F(2),
        _ => return None,
    };
    Some(KeyEvent::new(code, KeyModifiers::NONE))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn codes(bytes: &[u8]) -> Vec<KeyCode> {
        let mut decoder = KeyDecoder::default();
        decoder.push(bytes);
        let mut codes = Vec::new();
        while let Some(key) = decoder.pop() {
            codes.push(key.code);
        }
        codes
    }

    #[test]
    fn navigation_review_f2_decodes_across_every_chunk_boundary() {
        for sequence in [b"\x1bOQ".as_slice(), b"\x1b[12~".as_slice()] {
            for split in 1..sequence.len() {
                let mut decoder = KeyDecoder::default();
                decoder.push(&sequence[..split]);
                assert!(decoder.pop().is_none());
                decoder.push(&sequence[split..]);
                assert_eq!(decoder.pop().map(|key| key.code), Some(KeyCode::F(2)));
                assert!(decoder.pop().is_none());
            }
        }
    }

    #[test]
    fn navigation_keys_decode_without_a_shell_editor() {
        assert_eq!(
            codes(b"\x1b[A\x1b[B\x1b[5~\x1b[6~\rq"),
            vec![
                KeyCode::Up,
                KeyCode::Down,
                KeyCode::PageUp,
                KeyCode::PageDown,
                KeyCode::Enter,
                KeyCode::Char('q'),
            ]
        );
        assert_eq!(codes(b"\x1bOA"), vec![KeyCode::Up]);
    }

    #[test]
    fn interrupt_and_eof_bytes_stay_distinct() {
        let mut decoder = KeyDecoder::default();
        decoder.push(&[0x03, 0x04]);
        let first = decoder.pop().expect("ctrl-c");
        let second = decoder.pop().expect("ctrl-d");
        assert_eq!(first.code, KeyCode::Char('c'));
        assert!(first.modifiers.contains(KeyModifiers::CONTROL));
        assert_eq!(second.code, KeyCode::Char('d'));
        assert!(second.modifiers.contains(KeyModifiers::CONTROL));
    }

    #[test]
    fn split_escape_sequence_resolves_once_completed() {
        let mut decoder = KeyDecoder::default();
        decoder.push(b"\x1b");
        assert!(decoder.pop().is_none());
        decoder.push(b"[");
        assert!(decoder.pop().is_none());
        decoder.push(b"A");
        assert_eq!(decoder.pop().expect("arrow").code, KeyCode::Up);
    }

    #[test]
    fn lone_escape_returns_after_the_disambiguation_budget() {
        let mut decoder = KeyDecoder::default();
        decoder.push(b"\x1b");
        assert!(decoder.expire().is_none());
        decoder.since = Some(Instant::now() - ESCAPE_TIMEOUT - Duration::from_millis(1));
        assert_eq!(decoder.expire().expect("escape").code, KeyCode::Esc);
        assert!(decoder.pending.is_empty());
    }

    #[test]
    fn incomplete_unknown_sequence_is_dropped_without_leaking_fragments() {
        let mut decoder = KeyDecoder::default();
        decoder.push(b"\x1b[12");
        decoder.since = Some(Instant::now() - ESCAPE_TIMEOUT - Duration::from_millis(1));
        assert!(decoder.expire().is_none());
        assert!(decoder.pending.is_empty());
        decoder.push(b"\r");
        assert_eq!(decoder.pop().expect("enter").code, KeyCode::Enter);
    }

    #[test]
    fn overlong_sequence_is_discarded_instead_of_growing() {
        let mut decoder = KeyDecoder::default();
        decoder.push(b"\x1b[");
        decoder.push(&[b'1'; MAX_SEQUENCE_BYTES + 8]);
        assert!(decoder.pending.is_empty());
        assert!(decoder.pop().is_none());
    }

    #[test]
    fn bracketed_paste_is_ignored_across_batches() {
        let mut decoder = KeyDecoder::default();
        decoder.push(b"\x1b[200~rm -rf /\nq");
        assert!(decoder.pop().is_none());
        decoder.push(b"\x1b[201~");
        assert!(decoder.pop().is_none());
        decoder.push(b"\r");
        assert_eq!(decoder.pop().expect("enter").code, KeyCode::Enter);
        assert!(decoder.pop().is_none());
    }

    #[test]
    fn multibyte_text_never_leaks_partial_bytes_as_keys() {
        let mut decoder = KeyDecoder::default();
        decoder.push("你".as_bytes());
        assert!(decoder.pop().is_none());
        assert!(decoder.pending.is_empty());
        decoder.push(&[0xe4, 0xbd]);
        assert!(decoder.pop().is_none());
        decoder.push(&[0xa0, b'\r']);
        assert_eq!(decoder.pop().expect("enter").code, KeyCode::Enter);
    }

    #[test]
    fn discarding_pending_bytes_drops_the_previous_owner_queue() {
        let mut decoder = KeyDecoder::default();
        decoder.push(b"q\x1b[");
        decoder.clear();
        assert!(decoder.pop().is_none());
        assert!(decoder.pending.is_empty());
    }

    #[test]
    fn repeated_escape_still_returns() {
        let mut decoder = KeyDecoder::default();
        decoder.push(b"\x1b\x1b");
        assert_eq!(decoder.pop().expect("first escape").code, KeyCode::Esc);
        decoder.since = Some(Instant::now() - ESCAPE_TIMEOUT - Duration::from_millis(1));
        assert_eq!(decoder.expire().expect("second escape").code, KeyCode::Esc);
    }

    #[test]
    fn an_unterminated_paste_expires_instead_of_eating_the_keyboard() {
        let mut decoder = KeyDecoder::default();
        decoder.push(b"\x1b[200~partial paste");
        assert!(decoder.pop().is_none());
        decoder.since = Some(Instant::now() - ESCAPE_TIMEOUT - Duration::from_millis(1));
        assert!(decoder.expire().is_none());
        assert!(!decoder.pasting);
        decoder.push(&[0x03]);
        let key = decoder.pop().expect("ctrl-c after paste timeout");
        assert_eq!(key.code, KeyCode::Char('c'));
        assert!(key.modifiers.contains(KeyModifiers::CONTROL));
    }
}
