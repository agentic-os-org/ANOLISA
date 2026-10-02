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
}

/// Shares a prompt snapshot without reordering the raw-input event FIFO.
#[derive(Clone, Debug, Default)]
pub(crate) struct PromptEpochExchange(Arc<Mutex<PromptEpochState>>);

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

    pub(crate) fn publish(&self, epoch: u64, prompt: &[u8]) {
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
        state.slot = PromptEpochSlot::Ready(Arc::from(&prompt[retained_from..]));
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
}
