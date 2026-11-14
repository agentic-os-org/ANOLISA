//! Snapshot owner exit notifications and pending confirmation/bridge deadlines.

use super::Runtime;
use crate::{rejected, Result};
use std::{fs::File, sync::atomic::Ordering, sync::Arc, time::Instant};

/// Owned exit descriptors and the next pending owner deadline.
pub(crate) struct Maintenance {
    /// Descriptors retained until the accept loop finishes waiting.
    pub(crate) exits: Vec<Arc<File>>,
    /// Earliest confirmation or bridge-validity deadline.
    pub(crate) deadline: Option<Instant>,
}

impl Runtime {
    /// Snapshot live owners without retaining the registry lock during polling.
    pub(crate) fn maintenance(&self) -> Result<Maintenance> {
        let owners = self.owners.lock().map_err(|_| rejected("state_poisoned"))?;
        let mut exits = Vec::new();
        let mut deadline = None;
        for owner in owners.values() {
            // Cancelled preparation may still own its reservation. A readable
            // pidfd must not spin the accept loop while that worker drains.
            if owner.cancelled.load(Ordering::Acquire) {
                continue;
            }
            exits.extend(owner.process.exit_notification());
            if let Some(next) = owner.maintenance_deadline() {
                deadline = Some(deadline.map_or(next, |current: Instant| current.min(next)));
            }
        }
        Ok(Maintenance { exits, deadline })
    }
}
