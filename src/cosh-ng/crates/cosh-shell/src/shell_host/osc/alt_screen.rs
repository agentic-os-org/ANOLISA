// Owner: shell_host (alt-screen tracking, issue #2025). Extracted from
// osc.rs — which is past the 1000-line growth bar — per the #2168 review:
// accumulates DECSET/DECRST alternate-screen toggles from the passthrough
// stream for the interactive sentinel's fullscreen classification.

/// #2025: alternate-screen state accumulated from DECSET/DECRST
/// 1049/47/1047 in the passthrough stream. Sequences may split across
/// chunks, so a short tail carry bridges chunk boundaries.
#[derive(Debug, Default)]
pub(super) struct AltScreenTracker {
    active: bool,
    carry: Vec<u8>,
}

impl AltScreenTracker {
    /// Tracks toggles across chunk boundaries. The carry keeps the last
    /// few bytes of the previous chunk, so a toggle split between two
    /// reads is still observed; the toggle appearing latest in the
    /// combined window wins.
    pub(super) fn observe(&mut self, data: &[u8]) {
        const TOGGLES: [(&[u8], bool); 6] = [
            (b"\x1b[?1049h", true),
            (b"\x1b[?1049l", false),
            (b"\x1b[?1047h", true),
            (b"\x1b[?1047l", false),
            (b"\x1b[?47h", true),
            (b"\x1b[?47l", false),
        ];
        const MAX_SEQ: usize = 8;
        let mut window = std::mem::take(&mut self.carry);
        window.extend_from_slice(data);
        let mut latest: Option<(usize, bool)> = None;
        for (seq, active) in TOGGLES {
            let mut from = 0;
            while let Some(pos) = super::find_bytes(&window[from..], seq) {
                let at = from + pos;
                if latest.is_none_or(|(best, _)| at >= best) {
                    latest = Some((at, active));
                }
                from = at + seq.len();
            }
        }
        if let Some((_, active)) = latest {
            self.active = active;
        }
        let keep = window.len().min(MAX_SEQ - 1);
        self.carry = window[window.len() - keep..].to_vec();
    }

    /// Whether the foreground application currently owns the alternate
    /// screen (fullscreen TUI classification input).
    pub(super) fn active(&self) -> bool {
        self.active
    }
}

#[cfg(test)]
mod tests {
    use super::AltScreenTracker;

    #[test]
    fn tracks_toggles_across_chunk_boundaries_and_latest_wins() {
        let mut tracker = AltScreenTracker::default();
        assert!(!tracker.active());
        tracker.observe(b"\x1b[?1049h");
        assert!(tracker.active());
        // A toggle split between two reads is bridged by the carry.
        tracker.observe(b"tail\x1b[?10");
        tracker.observe(b"49l");
        assert!(!tracker.active());
        // The toggle appearing latest in the combined window wins.
        tracker.observe(b"\x1b[?47h...\x1b[?47l");
        assert!(!tracker.active());
        tracker.observe(b"\x1b[?1047l...\x1b[?1047h");
        assert!(tracker.active());
        // Unrelated output leaves the state untouched.
        tracker.observe(b"plain output");
        assert!(tracker.active());
    }
}
