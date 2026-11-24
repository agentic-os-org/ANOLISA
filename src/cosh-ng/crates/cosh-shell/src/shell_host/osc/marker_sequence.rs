//! Compact and legacy OSC marker envelopes plus history-file observation.

use std::path::PathBuf;

use serde::Deserialize;

use super::super::model::ShellHistoryFileObserver;

const SHELL_HISTORY_FILE_MAX_BYTES: usize = 4 * 1024;

pub(super) fn find_bytes(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    // A full match must start with the needle's first byte, so scanning for
    // that byte first skips the per-position slice comparisons of a naive
    // windows() walk on control-free output.
    let Some(&first) = needle.first() else {
        return Some(0);
    };
    let mut from = 0;
    while from + needle.len() <= haystack.len() {
        let skip = haystack[from..].iter().position(|byte| *byte == first)?;
        let at = from + skip;
        if haystack[at..].starts_with(needle) {
            return Some(at);
        }
        from = at + 1;
    }
    None
}

pub(super) fn osc_prefix_suffix_len(pending: &[u8]) -> usize {
    let max_keep = pending.len().min(super::OSC_PREFIX.len().saturating_sub(1));
    for size in (1..=max_keep).rev() {
        if super::OSC_PREFIX.starts_with(&pending[pending.len() - size..]) {
            return size;
        }
    }
    0
}

pub(super) fn resume_abandoned_prefix<'a>(
    pending: &mut Vec<u8>,
    abandoned_prefix_len: &mut usize,
    data: &'a [u8],
) -> Option<&'a [u8]> {
    if *abandoned_prefix_len == 0 {
        return Some(data);
    }
    let Some(continuation) = super::OSC_PREFIX.strip_prefix(pending.as_slice()) else {
        discard_abandoned_prefix(pending, abandoned_prefix_len);
        return Some(data);
    };
    if data.starts_with(continuation) {
        pending.extend_from_slice(continuation);
        *abandoned_prefix_len = 0;
        Some(&data[continuation.len()..])
    } else if continuation.starts_with(data) {
        pending.extend_from_slice(data);
        None
    } else {
        // Reparse the fresh chunk so abandoning a private fragment cannot
        // discard tentative continuation bytes that arrived after it.
        discard_abandoned_prefix(pending, abandoned_prefix_len);
        Some(data)
    }
}

pub(super) fn discard_abandoned_prefix(pending: &mut Vec<u8>, abandoned_prefix_len: &mut usize) {
    let private_len = std::mem::take(abandoned_prefix_len).min(pending.len());
    pending.drain(..private_len);
}

#[derive(Debug, Deserialize)]
pub(super) struct Marker {
    #[serde(alias = "e")]
    pub(super) event: String,
    #[serde(alias = "t")]
    pub(super) token: Option<String>,
    pub(super) session_id: Option<String>,
    pub(super) timestamp_ms: Option<u64>,
    #[serde(alias = "c")]
    pub(super) cwd: Option<String>,
    #[serde(alias = "pc")]
    pub(super) physical_cwd: Option<String>,
    #[serde(alias = "spn")]
    pub(super) shell_path_names: Option<Vec<String>>,
    #[serde(alias = "sps")]
    pub(super) shell_path_suffixes: Option<Vec<String>>,
    pub(super) command: Option<String>,
    pub(super) reason: Option<String>,
    #[serde(alias = "s")]
    pub(super) status: Option<i32>,
    pub(super) path: Option<String>,
    pub(super) path_trusted: Option<bool>,
    #[serde(alias = "h")]
    pub(super) history_file: Option<String>,
    pub(super) prompt_ready: Option<bool>,
    pub(super) generation: Option<u64>,
    pub(super) top_level_missing: Option<bool>,
    pub(super) proven: Option<bool>,
    pub(super) intent: Option<String>,
    pub(super) sensitive: Option<bool>,
    #[serde(rename = "unsafe")]
    pub(super) unsafe_input: Option<bool>,
    pub(super) legacy_bash_readline: Option<bool>,
    /// Handoff claim token echoed by the approved handoff marker pair.
    #[serde(alias = "x")]
    pub(super) handoff: Option<String>,
}

impl Marker {
    /// Expands the compact prompt boundary into the legacy parser event shape.
    pub(super) fn normalize_compact_prompt(&mut self) -> bool {
        if self.event != "p" {
            return false;
        }
        self.event = "precmd".to_string();
        self.prompt_ready = Some(true);
        true
    }

    pub(super) fn has_trusted_history_context(
        &self,
        session_id: &str,
        compact_prompt_marker: bool,
    ) -> bool {
        self.session_id.as_deref() == Some(session_id)
            || (compact_prompt_marker && self.session_id.is_none())
    }
}

#[derive(Debug, Default)]
pub(super) struct HistoryFileTracker {
    observer: Option<ShellHistoryFileObserver>,
    last_path: Option<PathBuf>,
}

impl HistoryFileTracker {
    pub(super) fn set_observer(&mut self, observer: ShellHistoryFileObserver) {
        self.observer = Some(observer);
    }

    pub(super) fn observe(&mut self, history_file: Option<&str>) {
        let Some(path) = history_file.filter(|value| {
            value.len() <= SHELL_HISTORY_FILE_MAX_BYTES
                && !value.chars().any(|character| character.is_control())
        }) else {
            return;
        };
        let path = PathBuf::from(path);
        if !path.is_absolute() || self.last_path.as_ref() == Some(&path) {
            return;
        }
        self.last_path = Some(path.clone());
        if let Some(observer) = &self.observer {
            observer.observe(path);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::find_bytes;

    fn naive(haystack: &[u8], needle: &[u8]) -> Option<usize> {
        haystack
            .windows(needle.len())
            .position(|window| window == needle)
    }

    #[test]
    fn accelerated_search_matches_the_naive_window_walk() {
        let haystacks: Vec<Vec<u8>> = vec![
            Vec::new(),
            b"a".to_vec(),
            b"aaaaab".to_vec(),
            b"\x1b]1337;COSH;{}".to_vec(),
            b"xx\x1b]1337;COSH;a\x1b]1337;COSH;b".to_vec(),
            b"\x1b\x1b\x1b]1337;COSH;".to_vec(),
            (0..=255u8).collect(),
            vec![b'1'; 5000],
            b"1\r\n2\r\n3\r\n".to_vec(),
        ];
        let needles: Vec<Vec<u8>> = vec![
            // The naive walk panics on a zero-length window, so the empty
            // needle is asserted separately below instead of compared here.
            b"a".to_vec(),
            b"\x1b".to_vec(),
            b"\x1b]1337;COSH;".to_vec(),
            b"\x1b[?1049h".to_vec(),
            b"aaab".to_vec(),
            b"zzz".to_vec(),
            vec![b'1'; 4000],
            vec![b'1'; 6000],
        ];
        for haystack in &haystacks {
            for needle in &needles {
                assert_eq!(
                    find_bytes(haystack, needle),
                    naive(haystack, needle),
                    "haystack_len={} needle_len={}",
                    haystack.len(),
                    needle.len()
                );
            }
        }
        assert_eq!(find_bytes(b"abc", b""), Some(0));
        assert_eq!(find_bytes(b"", b""), Some(0));
    }
}
