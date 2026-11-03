//! Shared byte reservations for probe admission and FFI callback ownership.

use crate::probes::ChannelWatermarks;
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

/// Shared byte-accounted admission for owned event payloads.
///
/// The channel's slot count cannot bound memory on its own: one SSL record
/// carries up to `MAX_BUF_SIZE` (4 MiB), so the 10 000 default slots admit
/// gigabytes of in-flight payload.
///
/// Zero disables the byte limit while retaining usage accounting.
#[derive(Clone)]
pub(crate) struct ChannelBudget {
    max_bytes: usize,
    in_flight: Arc<AtomicUsize>,
    max_count: usize,
    in_flight_count: Arc<AtomicUsize>,
    dropped_over_budget: Arc<AtomicUsize>,
    dropped_total: Arc<AtomicUsize>,
}

impl ChannelBudget {
    pub(crate) fn new(max_bytes: usize) -> Self {
        Self {
            max_bytes,
            in_flight: Arc::new(AtomicUsize::new(0)),
            max_count: usize::MAX,
            in_flight_count: Arc::new(AtomicUsize::new(0)),
            dropped_over_budget: Arc::new(AtomicUsize::new(0)),
            dropped_total: Arc::new(AtomicUsize::new(0)),
        }
    }

    /// Reserve `bytes` for an event about to be published.
    ///
    /// Must be called *before* the event becomes visible to the consumer: a
    /// post-send charge can lose the race against [`Self::release`] on the
    /// consumer side, and the saturating subtraction would then turn the late
    /// charge into a permanent phantom reservation that eventually wedges the
    /// budget shut. Returns false and counts a drop when over budget.
    pub(crate) fn reserve(&self, bytes: usize) -> bool {
        let max_bytes = self.max_bytes;
        let admitted = self
            .in_flight
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |cur| {
                // Saturating so a pathological size cannot wrap into a false pass.
                let next = cur.saturating_add(bytes);
                (max_bytes == 0 || next <= max_bytes).then_some(next)
            })
            .is_ok();
        if !admitted {
            self.dropped_over_budget.fetch_add(1, Ordering::Relaxed);
            self.dropped_total.fetch_add(1, Ordering::Relaxed);
        }
        admitted
    }

    /// Give back a reservation whose event never reached the channel.
    pub(crate) fn refund(&self, bytes: usize) {
        self.give_back(bytes);
    }

    /// Count actual input loss after every available bounded admission path failed.
    pub(crate) fn record_drop(&self) {
        self.dropped_total.fetch_add(1, Ordering::Relaxed);
    }

    /// Give back a reservation once the event has been delivered to the consumer.
    pub(crate) fn release(&self, bytes: usize) {
        self.give_back(bytes);
    }

    /// Saturating so an accounting mismatch cannot wrap the counter and lock out
    /// admission forever.
    pub(crate) fn give_back(&self, bytes: usize) {
        let _ = self
            .in_flight
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |cur| {
                Some(cur.saturating_sub(bytes))
            });
    }

    pub(crate) fn watermarks(&self) -> ChannelWatermarks {
        ChannelWatermarks {
            in_flight_bytes: self.in_flight.load(Ordering::Relaxed),
            budget_bytes: self.max_bytes,
            dropped_over_budget: self.dropped_over_budget.load(Ordering::Relaxed),
        }
    }

    pub(crate) fn dropped_total(&self) -> usize {
        self.dropped_total.load(Ordering::Relaxed)
    }
}

pub(crate) struct BudgetReservation {
    pub(crate) budget: ChannelBudget,
    pub(crate) bytes: usize,
    slots: usize,
}
impl Drop for BudgetReservation {
    fn drop(&mut self) {
        self.budget.release(self.bytes);
        self.budget
            .in_flight_count
            .fetch_sub(self.slots, Ordering::AcqRel);
    }
}
impl ChannelBudget {
    pub(crate) fn reserve_owned(&self, bytes: usize) -> Option<BudgetReservation> {
        self.reserve_many(bytes, 1)
    }
    pub(crate) fn reserve_many(&self, bytes: usize, slots: usize) -> Option<BudgetReservation> {
        if self
            .in_flight_count
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |current| {
                current
                    .checked_add(slots)
                    .filter(|next| *next <= self.max_count)
            })
            .is_err()
        {
            return None;
        }
        let admitted = self
            .in_flight
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |current| {
                let next = current.checked_add(bytes)?;
                (self.max_bytes == 0 || next <= self.max_bytes).then_some(next)
            })
            .is_ok();
        if !admitted {
            self.dropped_over_budget.fetch_add(1, Ordering::Relaxed);
            self.in_flight_count.fetch_sub(slots, Ordering::AcqRel);
        }
        admitted.then(|| BudgetReservation {
            budget: self.clone(),
            bytes,
            slots,
        })
    }
}
