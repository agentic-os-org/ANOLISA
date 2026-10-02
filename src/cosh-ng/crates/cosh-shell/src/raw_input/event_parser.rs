use crate::input::InputClassifier;

use super::path_prompt_candidate::zsh_path_candidate_should_hold;
use super::soft_newline::{
    soft_newline_sequence_len, soft_newline_suffix_len, CANONICAL_SOFT_NEWLINE,
};
use super::{CTRL_C, CTRL_U};

pub(super) const BRACKETED_PASTE_START: &[u8] = b"\x1b[200~";
pub(super) const BRACKETED_PASTE_END: &[u8] = b"\x1b[201~";

#[derive(Debug, Default)]
pub(super) struct CandidateLineBuffer {
    pub(super) bytes: Vec<u8>,
    pub(super) relayed_len: usize,
    pub(super) force_agent_intercept: bool,
    pub(super) forced_agent_suggestion_id: Option<String>,
    /// Whether soft-newline shortcuts are normalized on push. Enabled only on
    /// the escape-mode intercept path; the native path keeps pre-#1721 byte
    /// semantics (fail-closed default: disabled).
    pub(super) soft_newline_enabled: bool,
    /// Tracks whether the cursor is inside a bracketed-paste region so pasted
    /// newlines can be kept soft instead of submitting the prompt (#1721).
    in_paste: bool,
    /// True once any bracketed-paste opener was consumed into this draft.
    saw_paste: bool,
    /// Buffer offset immediately after the latest completed paste payload.
    /// Bytes after this offset were typed outside the paste wrapper.
    paste_closed_at: Option<usize>,
    /// Trailing bytes that form a proper prefix of a paste delimiter,
    /// held until the next chunk resolves them (#1721).
    pending_partial: Vec<u8>,
    /// A pasted CR ended the previous chunk; a leading LF in the next
    /// chunk folds into the same soft newline (#1721).
    pending_pasted_cr: bool,
}

impl CandidateLineBuffer {
    /// A bracketed paste is still streaming in: hold off routing decisions
    /// (card upgrade) until the closer arrives (#1721 D13).
    pub(super) fn in_paste(&self) -> bool {
        self.in_paste
    }

    /// Any part of the draft arrived via bracketed paste: when the line
    /// flushes back to bash the paste wrapper must be replayed so readline
    /// inserts the bytes instead of executing embedded newlines (#1721).
    pub(super) fn saw_paste(&self) -> bool {
        self.saw_paste
    }
}

impl CandidateLineBuffer {
    pub(super) fn is_active(&self) -> bool {
        // An isolated bracketed-paste opener strips to zero bytes but must
        // keep the candidate open so the payload chunk stays routed here
        // instead of leaking to the PTY (#1721); the same holds
        // for a held partial delimiter awaiting its second half.
        !self.bytes.is_empty() || self.in_paste || !self.pending_partial.is_empty()
    }

    /// Bytes held from a split delimiter, exposed so route decisions can
    /// evaluate the joined draft (#1721).
    pub(super) fn pending_partial_bytes(&self) -> &[u8] {
        &self.pending_partial
    }

    pub(super) fn push(&mut self, bytes: &[u8]) {
        // Re-join any held partial paste delimiter from the previous chunk
        // (#1721): PTY reads may split `\x1b[200~` / `\x1b[201~` at any
        // byte, and a half delimiter must never become draft content.
        let joined: Vec<u8>;
        let bytes: &[u8] = if self.pending_partial.is_empty() {
            bytes
        } else {
            let mut buffer = std::mem::take(&mut self.pending_partial);
            buffer.extend_from_slice(bytes);
            joined = buffer;
            &joined
        };
        let mut idx = 0;
        while idx < bytes.len() {
            // Fold a pasted CRLF split across reads into one newline
            // (#1721): the CR already emitted a canonical soft newline.
            if self.pending_pasted_cr {
                self.pending_pasted_cr = false;
                if self.in_paste && self.soft_newline_enabled && bytes[idx] == b'\n' {
                    idx += 1;
                    continue;
                }
            }
            if bytes[idx..].starts_with(BRACKETED_PASTE_START) {
                self.in_paste = true;
                self.saw_paste = true;
                self.paste_closed_at = None;
                idx += BRACKETED_PASTE_START.len();
                continue;
            }
            if bytes[idx..].starts_with(BRACKETED_PASTE_END) {
                self.in_paste = false;
                self.paste_closed_at = Some(self.bytes.len());
                idx += BRACKETED_PASTE_END.len();
                continue;
            }
            if is_partial_paste_delimiter(&bytes[idx..]) {
                // The whole tail is a proper prefix of a paste delimiter:
                // hold it for the next chunk (re-scanned on arrival; if it
                // turns out not to be a delimiter it is processed normally).
                self.pending_partial = bytes[idx..].to_vec();
                return;
            }
            if self.soft_newline_enabled {
                if self.in_paste && matches!(bytes[idx], b'\r' | b'\n') {
                    // Pasted newlines stay soft: rewrite to a whitelisted
                    // sequence so the first pasted line never auto-submits.
                    self.bytes.extend_from_slice(CANONICAL_SOFT_NEWLINE);
                    if bytes[idx] == b'\r' && idx + 1 == bytes.len() {
                        self.pending_pasted_cr = true;
                    }
                    idx += if bytes[idx..].starts_with(b"\r\n") {
                        2
                    } else {
                        1
                    };
                    continue;
                }
                if !self.in_paste {
                    if let Some(len) = soft_newline_sequence_len(&bytes[idx..]) {
                        // Keep the raw whitelisted bytes: the status scanner
                        // recognizes every whitelisted sequence in place, so
                        // sequences split across chunks normalize identically.
                        let sequence = &bytes[idx..idx + len];
                        self.bytes.extend_from_slice(sequence);
                        idx += len;
                        continue;
                    }
                }
            }
            // CSI-u Backspace deletes like 0x7f (#2150): terminals speaking
            // the extended keyboard protocol encode Backspace as `ESC [ 127 u`
            // with an optional numeric modifier.
            if let Some(len) = csi_u_backspace_len(&bytes[idx..]) {
                self.pop_visible_char();
                idx += len;
                continue;
            }
            match bytes[idx] {
                CTRL_U => {
                    self.clear();
                    idx += 1;
                }
                0x7f | 0x08 => {
                    self.pop_visible_char();
                    idx += 1;
                }
                0x1b if bytes.get(idx + 1) == Some(&b'[')
                    && bytes.get(idx + 2) == Some(&b'3')
                    && bytes.get(idx + 3) == Some(&b'~') =>
                {
                    self.pop_visible_char();
                    idx += 4;
                }
                byte => {
                    self.bytes.push(byte);
                    idx += 1;
                }
            }
        }
    }

    pub(super) fn clear(&mut self) {
        self.bytes.clear();
        self.relayed_len = 0;
        self.force_agent_intercept = false;
        self.forced_agent_suggestion_id = None;
        self.in_paste = false;
        self.saw_paste = false;
        self.paste_closed_at = None;
        self.pending_partial.clear();
        self.pending_pasted_cr = false;
    }

    pub(super) fn take(&mut self) -> Vec<u8> {
        self.relayed_len = 0;
        self.force_agent_intercept = false;
        self.forced_agent_suggestion_id = None;
        self.in_paste = false;
        self.saw_paste = false;
        self.paste_closed_at = None;
        self.pending_pasted_cr = false;
        let mut bytes = std::mem::take(&mut self.bytes);
        // A held partial delimiter is plain user bytes if the draft flushes
        // before the rest arrives: keep them byte-identical.
        bytes.extend_from_slice(&std::mem::take(&mut self.pending_partial));
        bytes
    }

    pub(super) fn visible_line_bytes(&self) -> &[u8] {
        &self.bytes[..visible_line_end(&self.bytes, self.soft_newline_enabled)]
    }

    pub(super) fn paste_closed_at(&self) -> Option<usize> {
        self.paste_closed_at
    }

    fn pop_visible_char(&mut self) {
        let end = visible_line_end(&self.bytes, self.soft_newline_enabled);
        if end == 0 {
            return;
        }
        if self.soft_newline_enabled {
            if let Some(len) = soft_newline_suffix_len(&self.bytes[..end]) {
                // A soft newline counts as one visible character.
                self.bytes.drain(end - len..end);
                return;
            }
        }
        let mut start = end - 1;
        while start > 0 && (self.bytes[start] & 0b1100_0000) == 0b1000_0000 {
            start -= 1;
        }
        self.bytes.drain(start..end);
    }
}

/// Length of the visible prefix: everything up to the first bare newline.
/// When soft newlines are enabled, whitelisted sequences are part of the
/// visible line and skipped whole so their CR/LF bytes are never mistaken
/// for a submit newline.
fn visible_line_end(bytes: &[u8], allow_soft_newline: bool) -> usize {
    let mut idx = 0;
    while idx < bytes.len() {
        if allow_soft_newline {
            if let Some(len) = soft_newline_sequence_len(&bytes[idx..]) {
                idx += len;
                continue;
            }
        }
        if matches!(bytes[idx], b'\n' | b'\r') {
            return idx;
        }
        idx += 1;
    }
    bytes.len()
}

#[derive(Debug, PartialEq, Eq)]
pub(super) enum CandidateLineStatus {
    Pending,
    Complete { line: String, line_len: usize },
    Unsafe,
}

#[derive(Clone, Debug, Default)]
pub(super) struct NativeLineState {
    visible: Vec<u8>,
    /// The mirror no longer matches readline's buffer: Tab completion or
    /// cursor-moving sequences edit the line where we cannot see (#1932).
    /// Reset together with the line (CR / Ctrl-C / Ctrl-U).
    dirty: bool,
    /// Inside a bracketed paste (the wrapper can span read chunks): pasted
    /// newlines stay in readline's buffer without submitting, which the
    /// single-line mirror cannot express, so they poison it (#1932).
    in_paste: bool,
    pending_paste_delimiter: Vec<u8>,
    multiline_paste_observed: bool,
}

impl NativeLineState {
    fn is_at_line_start(&self) -> bool {
        self.is_empty()
    }

    pub(super) fn is_empty(&self) -> bool {
        self.visible.is_empty()
            && !self.dirty
            && !self.in_paste
            && self.pending_paste_delimiter.is_empty()
    }

    /// The observed prompt-line bytes, only while the mirror is trusted
    /// (#1932 F6): `None` once an unobservable edit poisoned it.
    pub(super) fn clean_visible_line(&self) -> Option<&[u8]> {
        if self.dirty || self.in_paste || !self.pending_paste_delimiter.is_empty() {
            None
        } else {
            Some(&self.visible)
        }
    }

    pub(super) fn history_mirror_requires_fail_closed(&self) -> bool {
        self.dirty && !self.in_paste && self.pending_paste_delimiter.is_empty()
    }

    pub(super) fn paste_sequence_open(&self) -> bool {
        self.in_paste || !self.pending_paste_delimiter.is_empty()
    }

    pub(super) fn observe_shell_bytes(&mut self, bytes: &[u8]) {
        let joined;
        let bytes = if self.pending_paste_delimiter.is_empty() {
            bytes
        } else {
            let mut value = std::mem::take(&mut self.pending_paste_delimiter);
            value.extend_from_slice(bytes);
            joined = value;
            joined.as_slice()
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
                break;
            }
            if self.in_paste {
                // Paste payload is inserted verbatim by readline. A pasted
                // newline keeps composing inside readline's buffer, which
                // this single-line mirror cannot express: poison it instead
                // of collapsing the buffer to its last line (#1932).
                match bytes[idx] {
                    b'\n' | b'\r' => {
                        self.dirty = true;
                        self.multiline_paste_observed = true;
                    }
                    b'\t' => self.visible.push(b'\t'),
                    byte if byte < 0x20 || byte == 0x7f => self.dirty = true,
                    byte => self.visible.push(byte),
                }
                idx += 1;
                continue;
            }
            match bytes[idx] {
                CTRL_C | CTRL_U | b'\n' | b'\r' => {
                    self.clear();
                    idx += 1;
                }
                0x7f | 0x08 => {
                    self.pop_visible_char();
                    idx += 1;
                }
                0x1b if bytes.get(idx + 1) == Some(&b'[')
                    && bytes.get(idx + 2) == Some(&b'3')
                    && bytes.get(idx + 3) == Some(&b'~') =>
                {
                    // Delete removes the char under the cursor. While the
                    // mirror is clean the cursor sits at the end of the
                    // line (any cursor movement poisons it), so Delete is
                    // a readline no-op and the mirror must not shrink.
                    idx += 4;
                }
                b'\t' => {
                    self.dirty = true;
                    idx += 1;
                }
                byte if byte < 0x20 || byte == 0x1b => {
                    self.dirty = true;
                    idx += 1;
                }
                byte => {
                    self.visible.push(byte);
                    idx += 1;
                }
            }
        }
        if self.visible.len() > 4096 {
            self.visible.clear();
            self.dirty = true;
        }
    }

    pub(super) fn take_multiline_paste_observed(&mut self) -> bool {
        std::mem::take(&mut self.multiline_paste_observed)
    }

    pub(super) fn clear(&mut self) {
        self.visible.clear();
        self.dirty = false;
        self.in_paste = false;
        self.pending_paste_delimiter.clear();
        self.multiline_paste_observed = false;
    }

    fn pop_visible_char(&mut self) {
        if self.visible.is_empty() {
            return;
        }
        let mut start = self.visible.len() - 1;
        while start > 0 && (self.visible[start] & 0b1100_0000) == 0b1000_0000 {
            start -= 1;
        }
        self.visible.drain(start..);
    }
}

pub(super) fn candidate_inline_hint(line: &str) -> Option<String> {
    let trimmed = line.trim_start();
    if !trimmed.starts_with('/') || trimmed[1..].contains('/') {
        return None;
    }

    let mut parts = trimmed.split_whitespace();
    let token = parts.next().unwrap_or_default();
    match token {
        "/" => None,
        "/mode" if parts.next().is_none() => {
            Some("approval [recommend|auto|trust] | analysis [smart|auto|manual]".to_string())
        }
        "/details" if parts.next().is_none() => Some("<id>".to_string()),
        _ => {
            // List every matching command: returning only the first prefix
            // match hides same-prefix siblings (`/sta` hid `/stats`). Names
            // are deduplicated so multi-spec commands (`/mode`) still count
            // as a single candidate and keep their usage hint.
            let mut names: Vec<&'static str> = Vec::new();
            let mut sole_usage = None;
            for spec in crate::slash::registry::visible_slash_commands() {
                if spec.name.starts_with(token) && spec.name != token && !names.contains(&spec.name)
                {
                    names.push(spec.name);
                    if names.len() == 1 {
                        sole_usage = Some(spec.usage);
                    }
                }
            }
            match names.len() {
                0 => None,
                1 => sole_usage.map(str::to_string),
                _ => Some(names.join(" · ")),
            }
        }
    }
}

pub(crate) fn redact_extension_setting_value(input: &[u8]) -> Vec<u8> {
    let mut tokens = Vec::with_capacity(6);
    let mut index = 0;
    while index < input.len() && tokens.len() < 6 {
        while index < input.len() && input[index].is_ascii_whitespace() {
            index += 1;
        }
        if index == input.len() {
            break;
        }
        let start = index;
        while index < input.len() && !input[index].is_ascii_whitespace() {
            index += 1;
        }
        tokens.push((start, index));
    }

    if tokens.len() < 6
        || &input[tokens[0].0..tokens[0].1] != b"/extensions"
        || &input[tokens[1].0..tokens[1].1] != b"settings"
        || &input[tokens[2].0..tokens[2].1] != b"set"
    {
        return input.to_vec();
    }

    let value_start = tokens[5].0;
    let mut redacted = input.to_vec();
    for byte in &mut redacted[value_start..] {
        if !byte.is_ascii_whitespace() {
            *byte = b'*';
        }
    }
    redacted
}

pub(super) fn starts_native_intercept_candidate(
    bytes: &[u8],
    native_line_state: &NativeLineState,
    buffer_non_ascii_prefix: bool,
) -> bool {
    // Zsh also buffers a potential Han-leading path prompt because its ZLE
    // buffer cannot be cleared safely after submit-time classification.
    if !native_line_state.is_at_line_start() {
        return false;
    }
    let visible = first_visible_input_bytes(bytes);
    if visible.first() == Some(&b'/')
        || (buffer_non_ascii_prefix && zsh_path_candidate_should_hold(visible))
    {
        return true;
    }
    // A lone `?` may be the first half of a `??` marker typed key by key
    // (#1932): own the line now so the follow-up decides the route; any
    // non-`??` continuation flushes back to bash byte-identically.
    visible.starts_with(b"??") || visible == b"?"
}

/// The whole slice is a proper prefix of `\x1b[200~` / `\x1b[201~`
/// (#1721): PTY reads may split the delimiter itself at any byte.
fn is_partial_paste_delimiter(suffix: &[u8]) -> bool {
    !suffix.is_empty()
        && suffix.len() < BRACKETED_PASTE_START.len()
        && (BRACKETED_PASTE_START.starts_with(suffix) || BRACKETED_PASTE_END.starts_with(suffix))
}

/// Length of a whole CSI-u Backspace sequence at the head of `bytes`
/// (#2150): `ESC [ 127 u` or `ESC [ 127 ; <digits> u`. Any other
/// codepoint, colon sub-parameters, or a sequence split across reads
/// stays unrecognized and keeps the fail-closed Unsafe route.
pub(super) fn csi_u_backspace_len(bytes: &[u8]) -> Option<usize> {
    if !bytes.starts_with(b"\x1b[") {
        return None;
    }
    let param_len = bytes[2..]
        .iter()
        .take_while(|byte| matches!(byte, b'0'..=b'9' | b';'))
        .count();
    if bytes.get(2 + param_len) != Some(&b'u') {
        return None;
    }
    is_csi_u_backspace_params(&bytes[2..2 + param_len]).then_some(2 + param_len + 1)
}

/// CSI parameter body of a CSI-u Backspace (#2150): `127` alone or with
/// one numeric modifier (`127;<digits>`); readline does not distinguish
/// Backspace modifiers either, so all variants delete one character.
pub(super) fn is_csi_u_backspace_params(params: &[u8]) -> bool {
    match params.strip_prefix(b"127") {
        Some(b"") => true,
        Some([b';', modifier @ ..]) => {
            !modifier.is_empty() && modifier.iter().all(u8::is_ascii_digit)
        }
        _ => false,
    }
}

/// Only an explicit `??` candidate may compose soft newlines.
pub(super) fn native_candidate_allows_soft_newline(bytes: &[u8]) -> bool {
    match first_visible_input_byte(bytes) {
        Some(b'/') => false,
        Some(b'?') => first_visible_input_bytes(bytes).starts_with(b"??"),
        _ => false,
    }
}

fn first_visible_input_byte(bytes: &[u8]) -> Option<u8> {
    first_visible_input_bytes(bytes).first().copied()
}

fn first_visible_input_bytes(mut bytes: &[u8]) -> &[u8] {
    loop {
        if bytes.starts_with(BRACKETED_PASTE_START) {
            bytes = &bytes[BRACKETED_PASTE_START.len()..];
            continue;
        }
        if bytes.starts_with(BRACKETED_PASTE_END) {
            bytes = &bytes[BRACKETED_PASTE_END.len()..];
            continue;
        }
        return bytes;
    }
}

pub(super) fn native_candidate_should_return_to_shell(
    input_classifier: &InputClassifier,
    line_buffer: &CandidateLineBuffer,
) -> bool {
    let visible = line_buffer.visible_line_bytes();
    if visible.contains(&b'\t') {
        return true;
    }
    let Ok(line) = std::str::from_utf8(visible) else {
        return false;
    };
    let token = line.split_whitespace().next().unwrap_or_default();
    token.starts_with('/') && !input_classifier.is_slash_control_candidate(token)
}

pub(super) fn candidate_line_status(bytes: &[u8], allow_soft_newline: bool) -> CandidateLineStatus {
    if bytes.len() > 4096 {
        return CandidateLineStatus::Unsafe;
    }

    let mut idx = 0;
    while idx < bytes.len() {
        // Whitelisted soft-newline sequences are legal in-line content: skip
        // them whole so their CR/LF bytes never complete or poison the line
        // (#1721). The native path keeps them illegal (fail-closed,
        // pre-#1721 semantics).
        if allow_soft_newline {
            if let Some(len) = soft_newline_sequence_len(&bytes[idx..]) {
                idx += len;
                continue;
            }
        }
        let byte = bytes[idx];
        if byte == 0x1b {
            return if incomplete_escape_suffix(&bytes[idx..]) {
                CandidateLineStatus::Pending
            } else {
                CandidateLineStatus::Unsafe
            };
        }
        if matches!(byte, b'\n' | b'\r') {
            let line_len = idx + 1;
            let Some(line) = normalized_candidate_text(&bytes[..idx], allow_soft_newline) else {
                return CandidateLineStatus::Unsafe;
            };
            return CandidateLineStatus::Complete { line, line_len };
        }
        if byte < 0x20 && byte != b'\t' {
            return CandidateLineStatus::Unsafe;
        }
        idx += 1;
    }
    CandidateLineStatus::Pending
}

/// Renders buffer bytes as the submitted text: canonical soft newlines become
/// real newlines; any other control byte or invalid UTF-8 disqualifies the
/// line (mirrors the pre-#1721 fail-closed checks).
fn normalized_candidate_text(bytes: &[u8], allow_soft_newline: bool) -> Option<String> {
    let mut normalized = Vec::with_capacity(bytes.len());
    let mut idx = 0;
    while idx < bytes.len() {
        if allow_soft_newline {
            if let Some(len) = soft_newline_sequence_len(&bytes[idx..]) {
                normalized.push(b'\n');
                idx += len;
                continue;
            }
        }
        let byte = bytes[idx];
        if byte == 0x1b || (byte < 0x20 && !matches!(byte, b'\t')) {
            return None;
        }
        normalized.push(byte);
        idx += 1;
    }
    String::from_utf8(normalized).ok()
}

fn incomplete_escape_suffix(bytes: &[u8]) -> bool {
    match bytes {
        [0x1b] => true,
        [0x1b, b'[', parameters @ ..] => parameters.iter().all(|byte| matches!(byte, 0x20..=0x3f)),
        [0x1b, b'O'] => true,
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::{starts_native_intercept_candidate, NativeLineState};

    #[test]
    fn native_intercept_candidate_only_starts_at_line_start() {
        let mut state = NativeLineState::default();

        assert!(starts_native_intercept_candidate(b"/", &state, false));
        assert!(starts_native_intercept_candidate(
            b"?? hello",
            &state,
            false
        ));
        assert!(!starts_native_intercept_candidate(
            "你".as_bytes(),
            &state,
            false
        ));
        assert!(starts_native_intercept_candidate(
            "你".as_bytes(),
            &state,
            true
        ));
        state.observe_shell_bytes(b"vim .");
        assert!(!starts_native_intercept_candidate(b"/", &state, true));
        assert!(!starts_native_intercept_candidate(
            b"?? hello",
            &state,
            true
        ));
        assert!(!starts_native_intercept_candidate(
            "你".as_bytes(),
            &state,
            true
        ));

        state.observe_shell_bytes(b"\n");
        assert!(starts_native_intercept_candidate(b"/mode", &state, false));
    }
}
