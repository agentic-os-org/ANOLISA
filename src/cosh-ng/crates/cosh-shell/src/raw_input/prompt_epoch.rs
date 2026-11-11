//! Exchanges one bounded, quiescent prompt snapshot across the PTY boundary.

use std::sync::{Arc, Mutex};

const MAX_PROMPT_SNAPSHOT_BYTES: usize = 64 * 1024;

#[derive(Debug, Default)]
enum PromptEpochSlot {
    #[default]
    Closed,
    Open,
    Ready(Arc<[u8]>),
    Claimed(Arc<[u8]>),
    Missed,
}

#[derive(Debug, Default)]
struct PromptEpochState {
    epoch: u64,
    slot: PromptEpochSlot,
    starts_at_line_start: bool,
}

/// Shares a prompt snapshot without reordering the raw-input event FIFO.
#[derive(Clone, Debug, Default)]
pub(crate) struct PromptEpochExchange(Arc<Mutex<PromptEpochState>>);

#[derive(Clone, Debug)]
pub(crate) struct PromptSnapshot {
    exchange: PromptEpochExchange,
    epoch: u64,
    bytes: Arc<[u8]>,
    starts_at_line_start: bool,
}

impl PromptSnapshot {
    pub(crate) fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    pub(crate) fn starts_at_line_start(&self) -> bool {
        self.starts_at_line_start
    }

    #[cfg(test)]
    pub(crate) fn commit_is_locked(&self) -> bool {
        matches!(
            self.exchange.0.try_lock(),
            Err(std::sync::TryLockError::WouldBlock)
        )
    }

    pub(crate) fn with_ready<T>(&self, commit: impl FnOnce() -> T) -> Option<T> {
        let state = self
            .exchange
            .0
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if state.epoch != self.epoch || !matches!(state.slot, PromptEpochSlot::Ready(_)) {
            return None;
        }
        let result = commit();
        drop(state);
        Some(result)
    }
}

impl PromptEpochExchange {
    pub(crate) fn open(&self) -> u64 {
        let mut state = self
            .0
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        state.epoch = state.epoch.wrapping_add(1).max(1);
        state.slot = PromptEpochSlot::Open;
        state.epoch
    }

    #[cfg(test)]
    pub(crate) fn publish(&self, epoch: u64, prompt: &[u8]) {
        self.publish_with_origin(epoch, prompt, false);
    }

    pub(crate) fn publish_with_origin(
        &self,
        epoch: u64,
        prompt: &[u8],
        starts_at_line_start: bool,
    ) {
        if prompt.is_empty() {
            return;
        }
        let mut state = self
            .0
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if state.epoch != epoch || !matches!(state.slot, PromptEpochSlot::Open) {
            return;
        }
        let retained_from = prompt.len().saturating_sub(MAX_PROMPT_SNAPSHOT_BYTES);
        state.starts_at_line_start = starts_at_line_start && retained_from == 0;
        state.slot = PromptEpochSlot::Ready(Arc::from(&prompt[retained_from..]));
    }

    pub(crate) fn peek_ready(&self, epoch: u64) -> Option<Arc<[u8]>> {
        let state = self
            .0
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if state.epoch != epoch {
            return None;
        }
        match &state.slot {
            PromptEpochSlot::Ready(prompt) => Some(Arc::clone(prompt)),
            _ => None,
        }
    }

    pub(crate) fn ready_snapshot(&self, epoch: u64) -> Option<PromptSnapshot> {
        let state = self
            .0
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if state.epoch != epoch {
            return None;
        }
        let PromptEpochSlot::Ready(bytes) = &state.slot else {
            return None;
        };
        Some(PromptSnapshot {
            exchange: self.clone(),
            epoch,
            bytes: Arc::clone(bytes),
            starts_at_line_start: state.starts_at_line_start,
        })
    }

    pub(crate) fn claim_before_user_write(&self) {
        let mut state = self
            .0
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        state.slot = match std::mem::take(&mut state.slot) {
            PromptEpochSlot::Ready(prompt) => PromptEpochSlot::Claimed(prompt),
            PromptEpochSlot::Open => PromptEpochSlot::Missed,
            slot => slot,
        };
    }

    pub(crate) fn take_claimed(&self, epoch: u64) -> Option<Arc<[u8]>> {
        let mut state = self
            .0
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if state.epoch != epoch {
            return None;
        }
        let PromptEpochSlot::Claimed(prompt) = std::mem::take(&mut state.slot) else {
            return None;
        };
        state.slot = PromptEpochSlot::Missed;
        Some(prompt)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ready_prompt_is_claimed_once_and_survives_later_writes() {
        let exchange = PromptEpochExchange::default();
        let first = exchange.open();
        exchange.publish(first, b"prompt$ ");

        exchange.claim_before_user_write();
        exchange.claim_before_user_write();
        assert_eq!(
            exchange.take_claimed(first).as_deref(),
            Some(b"prompt$ ".as_slice())
        );
        assert!(exchange.take_claimed(first).is_none());

        let second = exchange.open();
        assert_ne!(second, first);
        assert!(exchange.take_claimed(first).is_none());
    }

    #[test]
    fn input_before_quiescent_publish_fails_quiet() {
        let exchange = PromptEpochExchange::default();
        let epoch = exchange.open();
        exchange.claim_before_user_write();
        exchange.publish(epoch, b"prompt$ polluted");

        assert!(exchange.take_claimed(epoch).is_none());
    }

    #[test]
    fn prompt_snapshot_is_bounded_at_publication() {
        let exchange = PromptEpochExchange::default();
        let epoch = exchange.open();
        let prompt = vec![b'p'; MAX_PROMPT_SNAPSHOT_BYTES + 17];
        exchange.publish(epoch, &prompt);
        exchange.claim_before_user_write();

        let claimed = exchange.take_claimed(epoch).expect("claimed prompt");
        assert_eq!(claimed.len(), MAX_PROMPT_SNAPSHOT_BYTES);
        assert_eq!(claimed.as_ref(), &prompt[17..]);
    }

    #[test]
    fn snapshot_origin_requires_proof_and_cannot_survive_truncation() {
        let exchange = PromptEpochExchange::default();
        let epoch = exchange.open();
        exchange.publish(epoch, b"native$ ");
        assert!(!exchange
            .ready_snapshot(epoch)
            .unwrap()
            .starts_at_line_start());
        let epoch = exchange.open();
        exchange.publish_with_origin(epoch, b"native$ ", true);
        assert!(exchange
            .ready_snapshot(epoch)
            .unwrap()
            .starts_at_line_start());
        let epoch = exchange.open();
        exchange.publish_with_origin(epoch, &vec![b'p'; MAX_PROMPT_SNAPSHOT_BYTES + 1], true);
        assert!(!exchange
            .ready_snapshot(epoch)
            .unwrap()
            .starts_at_line_start());
    }
    #[test]
    fn read_only_prompt_peek_observes_only_published_unclaimed_same_epoch() {
        let exchange = PromptEpochExchange::default();
        let epoch = exchange.open();
        assert!(exchange.peek_ready(epoch).is_none());
        exchange.publish(epoch, b"native$ ");
        assert_eq!(
            exchange.peek_ready(epoch).as_deref(),
            Some(b"native$ ".as_slice())
        );
        assert_eq!(
            exchange.peek_ready(epoch).as_deref(),
            Some(b"native$ ".as_slice())
        );
        assert!(exchange.peek_ready(epoch + 1).is_none());
        exchange.claim_before_user_write();
        assert!(exchange.peek_ready(epoch).is_none());
        assert_eq!(
            exchange.take_claimed(epoch).as_deref(),
            Some(b"native$ ".as_slice())
        );
    }
}

#[cfg(test)]
mod commit_tests {
    use super::*;
    use std::sync::mpsc;
    use std::time::Duration;

    #[test]
    fn prompt_commit_holds_the_existing_user_claim_boundary_until_display_finishes() {
        let generation = crate::raw_input::UserPtyInputGeneration::default();
        let exchange = generation.prompt_epoch_exchange();
        let epoch = exchange.open();
        exchange.publish(epoch, b"native$ ");
        let snapshot = exchange.ready_snapshot(epoch).unwrap();
        let (attempt_tx, attempt_rx) = mpsc::sync_channel(0);
        let (resume_tx, resume_rx) = mpsc::sync_channel(0);
        let (written_tx, written_rx) = mpsc::channel();
        let check_exchange = exchange.clone();
        let writer = std::thread::spawn(move || {
            resume_rx.recv().unwrap();
            assert!(matches!(
                check_exchange.0.try_lock(),
                Err(std::sync::TryLockError::WouldBlock)
            ));
            attempt_tx.send(()).unwrap();
            generation.bump();
            written_tx.send(()).unwrap();
        });
        snapshot
            .with_ready(|| {
                resume_tx.send(()).unwrap();
                attempt_rx.recv_timeout(Duration::from_secs(2)).unwrap();
                assert!(matches!(
                    written_rx.try_recv(),
                    Err(mpsc::TryRecvError::Empty)
                ));
            })
            .unwrap();
        written_rx.recv_timeout(Duration::from_secs(2)).unwrap();
        writer.join().unwrap();
        assert!(snapshot
            .with_ready(|| panic!("already claimed prompt committed twice"))
            .is_none());
        assert_eq!(
            exchange.take_claimed(epoch).as_deref(),
            Some(b"native$ ".as_slice())
        );
    }

    #[test]
    fn replacement_epoch_cannot_commit_an_old_prompt_token() {
        let exchange = PromptEpochExchange::default();
        let epoch = exchange.open();
        exchange.publish(epoch, b"old$ ");
        let snapshot = exchange.ready_snapshot(epoch).unwrap();
        let next = exchange.open();
        exchange.publish(next, b"new$ ");
        assert!(snapshot
            .with_ready(|| panic!("stale epoch accepted"))
            .is_none());
        assert_eq!(
            exchange.peek_ready(next).as_deref(),
            Some(b"new$ ".as_slice())
        );
    }
}
