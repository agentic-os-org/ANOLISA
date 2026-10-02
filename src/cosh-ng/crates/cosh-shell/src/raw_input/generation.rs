//! Shared generation counter for real user bytes written to the shell PTY.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use super::PromptEpochExchange;

/// Monotonic counter shared between the input relay and the PTY output loop.
///
/// The relay bumps it *before* writing user bytes to the PTY master, so any
/// PTY output produced in response to those bytes is only readable after the
/// bump. The output loop compares snapshots against this counter to expire
/// stale prompt-replay state without depending on channel drain timing.
#[derive(Debug, Clone, Default)]
pub(crate) struct UserPtyInputGeneration {
    counter: Arc<AtomicU64>,
    prompt_epoch: PromptEpochExchange,
}

impl UserPtyInputGeneration {
    /// Records one batch of user bytes about to reach the PTY; returns the
    /// new generation.
    pub(crate) fn bump(&self) -> u64 {
        self.prompt_epoch.claim_before_user_write();
        self.counter.fetch_add(1, Ordering::SeqCst) + 1
    }

    pub(crate) fn current(&self) -> u64 {
        self.counter.load(Ordering::SeqCst)
    }

    pub(crate) fn prompt_epoch_exchange(&self) -> PromptEpochExchange {
        self.prompt_epoch.clone()
    }
}

const BRACKETED_PASTE_START: &[u8] = b"\x1b[200~";
const BRACKETED_PASTE_END: &[u8] = b"\x1b[201~";
// Default `operate-and-get-next` binding: accepts the current line exactly
// like Enter, so it must be treated as a submission candidate.
const CTRL_O: u8 = 0x0f;

/// Counts line-submission candidates in relayed user bytes.
///
/// This is a heuristic over-approximation of readline's logical accept-line:
/// CR, LF, and Ctrl-O (bash's default `operate-and-get-next`) count. Readline
/// functions that execute an editor-written buffer (`edit-and-execute-command`
/// and `vi-edit-and-execute-command`) and custom `.inputrc` submission bindings
/// expose no reliable accept boundary in these bytes and stay uncovered.
/// Over-counts (e.g. Ctrl-O typed inside a foreground program) are reconciled
/// by the prompt-replay tracker once the shell idles at a prompt.
///
/// Newlines inside a bracketed paste are inserted literally by readline and
/// never produce a prompt boundary, so they are excluded. Paste delimiters
/// split across writes are buffered until they can be classified.
#[derive(Clone, Debug, Default)]
pub(super) struct LineSubmitCounter {
    in_bracketed_paste: bool,
    pending_delimiter: Vec<u8>,
}

impl LineSubmitCounter {
    pub(super) fn count(&mut self, bytes: &[u8]) -> usize {
        self.scan(bytes, false, |_| {}).0
    }

    /// Returns the first submission byte in this write without advancing the
    /// counter's cross-write bracketed-paste state.
    pub(super) fn first_submission(&self, bytes: &[u8]) -> Option<usize> {
        let mut scanner = self.clone();
        scanner.scan(bytes, true, |_| {}).1
    }

    /// Returns every submission byte without advancing cross-write state.
    pub(super) fn submission_positions(&self, bytes: &[u8]) -> Vec<usize> {
        let mut scanner = self.clone();
        let mut positions = Vec::new();
        scanner.scan(bytes, false, |position| positions.push(position));
        positions
    }

    fn scan(
        &mut self,
        bytes: &[u8],
        stop_after_first: bool,
        mut on_submission: impl FnMut(usize),
    ) -> (usize, Option<usize>) {
        let pending_len = self.pending_delimiter.len();
        let mut scan = std::mem::take(&mut self.pending_delimiter);
        scan.extend_from_slice(bytes);
        let mut submits = 0;
        let mut first = None;
        let mut idx = 0;
        while idx < scan.len() {
            let rest = &scan[idx..];
            if rest.starts_with(BRACKETED_PASTE_START) {
                self.in_bracketed_paste = true;
                idx += BRACKETED_PASTE_START.len();
                continue;
            }
            if rest.starts_with(BRACKETED_PASTE_END) {
                self.in_bracketed_paste = false;
                idx += BRACKETED_PASTE_END.len();
                continue;
            }
            if is_paste_delimiter_prefix(rest) {
                // Keep the partial delimiter for the next write; ESC-sequence
                // prefixes never contain CR/LF, so nothing is under-counted.
                self.pending_delimiter = rest.to_vec();
                return (submits, first);
            }
            if matches!(scan[idx], b'\r' | b'\n' | CTRL_O) && !self.in_bracketed_paste {
                let position = idx - pending_len;
                submits += 1;
                first.get_or_insert(position);
                on_submission(position);
                if stop_after_first {
                    return (submits, first);
                }
            }
            idx += 1;
        }
        (submits, first)
    }
}

fn is_paste_delimiter_prefix(bytes: &[u8]) -> bool {
    bytes.len() < BRACKETED_PASTE_START.len()
        && (BRACKETED_PASTE_START.starts_with(bytes) || BRACKETED_PASTE_END.starts_with(bytes))
}

#[cfg(test)]
mod tests {
    use super::{LineSubmitCounter, UserPtyInputGeneration};

    #[test]
    fn bump_is_visible_through_clones() {
        let generation = UserPtyInputGeneration::default();
        let shared = generation.clone();

        assert_eq!(generation.current(), 0);
        assert_eq!(shared.bump(), 1);
        assert_eq!(generation.current(), 1);
    }

    #[test]
    fn bump_claims_ready_prompt_before_advancing_generation() {
        let generation = UserPtyInputGeneration::default();
        let exchange = generation.prompt_epoch_exchange();
        let epoch = exchange.open();
        exchange.publish(epoch, b"prompt$ ");

        assert_eq!(generation.bump(), 1);
        assert_eq!(
            exchange.take_claimed(epoch).as_deref(),
            Some(b"prompt$ ".as_slice())
        );
    }

    #[test]
    fn line_submit_counter_counts_cr_and_lf() {
        let mut counter = LineSubmitCounter::default();

        assert_eq!(counter.count(b"ls -la\r"), 1);
        assert_eq!(counter.count(b"\r\r"), 2);
        assert_eq!(counter.count(b"abc"), 0);
    }

    #[test]
    fn line_submit_counter_counts_ctrl_o_as_submission() {
        let mut counter = LineSubmitCounter::default();

        assert_eq!(counter.count(b"ls -la\x0f"), 1);
        assert_eq!(counter.count(b"\x1b[200~\x0f\x1b[201~"), 0);
    }

    #[test]
    fn line_submit_counter_ignores_newlines_inside_bracketed_paste() {
        let mut counter = LineSubmitCounter::default();

        assert_eq!(counter.count(b"\x1b[200~line1\nline2\x1b[201~\r"), 1);
    }

    #[test]
    fn line_submit_counter_tracks_paste_state_across_writes() {
        let mut counter = LineSubmitCounter::default();

        assert_eq!(counter.count(b"\x1b[200~one\n"), 0);
        assert_eq!(counter.count(b"two\n\x1b[201~"), 0);
        assert_eq!(counter.count(b"\r"), 1);
    }

    #[test]
    fn line_submit_counter_buffers_split_paste_delimiter() {
        let mut counter = LineSubmitCounter::default();

        assert_eq!(counter.count(b"\x1b[20"), 0);
        assert_eq!(counter.count(b"0~in\n"), 0);
        assert_eq!(counter.count(b"\x1b[201~\r"), 1);
    }

    #[test]
    fn first_submission_uses_the_same_paste_aware_boundaries() {
        let counter = LineSubmitCounter::default();

        assert_eq!(counter.first_submission(b"command\x0f"), Some(7));
        assert_eq!(
            counter.first_submission(b"\x1b[200~line1\nline2\x1b[201~\r"),
            Some(23)
        );
        assert_eq!(
            counter.first_submission(b"\x1b[200~line1\nline2\x1b[201~"),
            None
        );
    }

    #[test]
    fn submission_positions_keep_batch_boundaries_outside_pastes() {
        let counter = LineSubmitCounter::default();

        assert_eq!(counter.submission_positions(b"one\ntwo\r"), vec![3, 7]);
        assert_eq!(
            counter.submission_positions(b"one\n\x1b[200~two\nthree\x1b[201~\r"),
            vec![3, 25]
        );
    }

    #[test]
    fn first_submission_honors_split_paste_delimiters_without_mutation() {
        let mut counter = LineSubmitCounter::default();
        assert_eq!(counter.count(b"\x1b[20"), 0);

        assert_eq!(counter.first_submission(b"0~line1\n"), None);
        assert_eq!(counter.first_submission(b"0~line1\n"), None);
        assert_eq!(counter.count(b"0~line1\n"), 0);
        assert_eq!(counter.first_submission(b"\x1b[201~\x0f"), Some(6));
    }
}
