//! Monotonic evaluation limits derived once from the original wall-clock budget.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use tokio::time::Instant;

use crate::contract::Budget;
use crate::protocol::ContractError;

/// Captured before input decoding so preparation cannot renew the event budget.
#[derive(Clone, Copy, Debug)]
pub struct Ingress {
    instant: Instant,
    unix_ms: u64,
}

impl Ingress {
    /// Capture the local clock pair without accepting a caller-supplied check time.
    pub fn capture() -> Result<Self, ContractError> {
        let instant = Instant::now();
        let unix_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|_| ContractError::InvalidInput)?
            .as_millis()
            .try_into()
            .map_err(|_| ContractError::InvalidInput)?;
        Ok(Self { instant, unix_ms })
    }

    /// Unix time advanced only by monotonic elapsed time after capture.
    pub fn now_ms(&self) -> u64 {
        self.unix_ms.saturating_add(self.elapsed_ms())
    }

    /// Preparation and evaluation latency, excluding no intermediate stages.
    pub fn elapsed_ms(&self) -> u64 {
        self.instant
            .elapsed()
            .as_millis()
            .try_into()
            .unwrap_or(u64::MAX)
    }

    /// Bound input waiting even before a complete request can be decoded.
    pub fn input_deadline(&self) -> Instant {
        self.preparation_deadline(120_000)
    }

    /// Apply a configuration cap without restarting time at the end of configuration loading.
    pub fn preparation_deadline(&self, timeout_ms: u64) -> Instant {
        self.instant + Duration::from_millis(timeout_ms.min(120_000))
    }

    /// Check the original event and local cap before emitting an allowance.
    pub fn expired(&self, budget: &Budget, timeout_ms: u64) -> bool {
        self.limit(budget, timeout_ms).expired()
    }

    /// Bound transport by the original event or evidence expiry without renewing time.
    pub fn event_deadline_at(&self, expires_at_ms: u64) -> Instant {
        let remaining = expires_at_ms.saturating_sub(self.unix_ms);
        self.instant + Duration::from_millis(remaining.min(120_000))
    }

    /// Apply a local timeout cap to the original event or evidence expiry.
    pub fn deadline_at(&self, expires_at_ms: u64, timeout_ms: u64) -> Instant {
        self.event_deadline_at(expires_at_ms)
            .min(self.instant + Duration::from_millis(timeout_ms))
    }

    pub(crate) fn limit(&self, budget: &Budget, timeout_ms: u64) -> Deadline {
        Deadline {
            ingress: *self,
            end: self.deadline_at(budget.deadline_ms, timeout_ms),
        }
    }
}

pub(crate) struct Deadline {
    pub ingress: Ingress,
    pub end: Instant,
}

impl Deadline {
    pub fn expired(&self) -> bool {
        Instant::now() >= self.end
    }
}
