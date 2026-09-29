use crate::CheckpointOutcome;

const NANOS_PER_DAY: u64 = 24 * 60 * 60 * 1_000_000_000;

/// Selects which size measurement drives a policy threshold.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SizeBasis {
    /// Main database, WAL, and SHM bytes on disk.
    Physical,
    /// Physical bytes minus reusable SQLite freelist pages.
    Logical,
}

/// Size enforcement parameters independent of database schema.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SizePolicy {
    /// Reference maximum used to scale adaptive deletion batches.
    pub limit_bytes: u64,
    /// Size at which maintenance begins.
    pub trigger_bytes: u64,
    /// Size at or below which maintenance stops.
    pub target_bytes: u64,
    /// Measurement used for the trigger comparison.
    pub trigger_basis: SizeBasis,
    /// Measurement used for convergence.
    pub target_basis: SizeBasis,
    /// Maximum deletion rounds per invocation.
    pub max_rounds: u32,
    /// Consecutive non-decreasing rounds allowed before stopping.
    pub max_stalled_rounds: u32,
}

impl SizePolicy {
    /// Validates the policy before it is used.
    ///
    /// # Errors
    ///
    /// Returns an error when enabled thresholds or round limits are inconsistent.
    pub fn validate(self) -> Result<Self, crate::LifecycleError> {
        if self.limit_bytes == 0 {
            return Ok(self);
        }
        if self.trigger_bytes == 0 || self.target_bytes == 0 {
            return Err(crate::LifecycleError::InvalidPolicy(
                "enabled size policy requires non-zero trigger and target",
            ));
        }
        if self.target_bytes > self.trigger_bytes {
            return Err(crate::LifecycleError::InvalidPolicy(
                "target bytes must not exceed trigger bytes",
            ));
        }
        if self.max_rounds == 0 || self.max_stalled_rounds == 0 {
            return Err(crate::LifecycleError::InvalidPolicy(
                "enabled size policy requires non-zero round limits",
            ));
        }
        Ok(self)
    }
}

/// Why one size-maintenance invocation stopped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MaintenanceStatus {
    /// Size enforcement is disabled.
    Disabled,
    /// The trigger threshold was not exceeded.
    BelowTrigger,
    /// Physical allocation is high but logical data already fits the target.
    ReusableCapacity,
    /// Deletion brought logical data to the requested target.
    TargetReached,
    /// The business store had no rows left to delete.
    NoRows,
    /// A reader prevented WAL truncation, so further deletes would not converge.
    CheckpointBusy,
    /// Measurements stopped decreasing across the configured number of rounds.
    Stalled,
    /// The configured maximum number of rounds was reached.
    MaxRounds,
}

/// Outcome of one size-maintenance invocation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MaintenanceReport {
    /// Terminal status.
    pub status: MaintenanceStatus,
    /// Number of deletion rounds attempted.
    pub rounds: u32,
    /// Total rows deleted by the business store.
    pub deleted_rows: usize,
    /// Size before maintenance.
    pub before: crate::SizeSnapshot,
    /// Last measured size.
    pub after: crate::SizeSnapshot,
}

/// Runs schema-independent size enforcement through business-owned callbacks.
///
/// The delete callback receives the fraction of each eligible table to remove.
/// The caller remains responsible for deletion order and referential integrity.
///
/// # Errors
///
/// Returns callback errors without hiding SQLite or business-store failures.
pub fn enforce_size_policy<E>(
    policy: SizePolicy,
    mut measure: impl FnMut() -> Result<crate::SizeSnapshot, E>,
    mut delete_oldest: impl FnMut(f64) -> Result<usize, E>,
    mut checkpoint: impl FnMut() -> Result<CheckpointOutcome, E>,
) -> Result<MaintenanceReport, E>
where
    E: From<crate::LifecycleError>,
{
    let policy = policy.validate().map_err(E::from)?;
    let before = measure()?;
    if policy.limit_bytes == 0 {
        return Ok(report(MaintenanceStatus::Disabled, 0, 0, before, before));
    }
    if size_for(before, policy.trigger_basis) <= policy.trigger_bytes {
        return Ok(report(
            MaintenanceStatus::BelowTrigger,
            0,
            0,
            before,
            before,
        ));
    }
    if size_for(before, policy.target_basis) <= policy.target_bytes {
        return Ok(report(
            MaintenanceStatus::ReusableCapacity,
            0,
            0,
            before,
            before,
        ));
    }

    let mut current = before;
    let mut deleted_rows = 0usize;
    let mut stalled_rounds = 0u32;
    let mut fraction_boost = 1.0f64;

    for round in 1..=policy.max_rounds {
        let current_size = size_for(current, policy.target_basis);
        let overshoot = current_size as f64 / policy.limit_bytes as f64;
        let base_fraction = if overshoot > 5.0 {
            0.50
        } else if overshoot > 2.0 {
            0.25
        } else {
            0.10
        };
        let deleted = delete_oldest((base_fraction * fraction_boost).min(0.9))?;
        deleted_rows = deleted_rows.saturating_add(deleted);
        if deleted == 0 {
            return Ok(report(
                MaintenanceStatus::NoRows,
                round,
                deleted_rows,
                before,
                current,
            ));
        }

        if checkpoint()? == CheckpointOutcome::Busy {
            return Ok(report(
                MaintenanceStatus::CheckpointBusy,
                round,
                deleted_rows,
                before,
                current,
            ));
        }

        let next = measure()?;
        if size_for(next, policy.target_basis) <= policy.target_bytes {
            return Ok(report(
                MaintenanceStatus::TargetReached,
                round,
                deleted_rows,
                before,
                next,
            ));
        }

        if size_for(next, policy.target_basis) < current_size {
            stalled_rounds = 0;
            fraction_boost = 1.0;
        } else {
            stalled_rounds += 1;
            fraction_boost = (fraction_boost * 2.0).min(9.0);
            if stalled_rounds >= policy.max_stalled_rounds {
                return Ok(report(
                    MaintenanceStatus::Stalled,
                    round,
                    deleted_rows,
                    before,
                    next,
                ));
            }
        }
        current = next;
    }

    Ok(report(
        MaintenanceStatus::MaxRounds,
        policy.max_rounds,
        deleted_rows,
        before,
        current,
    ))
}

/// Computes an age-retention cutoff from a nanosecond timestamp.
///
/// # Errors
///
/// Returns an error when the retention duration overflows `u64` nanoseconds.
pub fn retention_cutoff_ns(now_ns: u64, retention_days: u64) -> Result<u64, crate::LifecycleError> {
    let retention_ns = retention_days
        .checked_mul(NANOS_PER_DAY)
        .ok_or(crate::LifecycleError::RetentionOverflow)?;
    Ok(now_ns.saturating_sub(retention_ns))
}

fn size_for(snapshot: crate::SizeSnapshot, basis: SizeBasis) -> u64 {
    match basis {
        SizeBasis::Physical => snapshot.physical_bytes,
        SizeBasis::Logical => snapshot.logical_bytes,
    }
}

fn report(
    status: MaintenanceStatus,
    rounds: u32,
    deleted_rows: usize,
    before: crate::SizeSnapshot,
    after: crate::SizeSnapshot,
) -> MaintenanceReport {
    MaintenanceReport {
        status,
        rounds,
        deleted_rows,
        before,
        after,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snapshot(physical_bytes: u64, logical_bytes: u64) -> crate::SizeSnapshot {
        crate::SizeSnapshot {
            database_bytes: physical_bytes,
            wal_bytes: 0,
            shm_bytes: 0,
            freelist_bytes: physical_bytes.saturating_sub(logical_bytes),
            physical_bytes,
            logical_bytes,
        }
    }

    fn policy() -> SizePolicy {
        SizePolicy {
            limit_bytes: 100,
            trigger_bytes: 100,
            target_bytes: 90,
            trigger_basis: SizeBasis::Physical,
            target_basis: SizeBasis::Logical,
            max_rounds: 5,
            max_stalled_rounds: 2,
        }
    }

    #[test]
    fn skips_physical_allocation_that_is_reusable() {
        let report = enforce_size_policy::<crate::LifecycleError>(
            policy(),
            || Ok(snapshot(120, 80)),
            |_| panic!("delete must not run"),
            || panic!("checkpoint must not run"),
        )
        .unwrap();
        assert_eq!(report.status, MaintenanceStatus::ReusableCapacity);
    }

    #[test]
    fn stops_when_checkpoint_is_busy() {
        let report = enforce_size_policy::<crate::LifecycleError>(
            policy(),
            || Ok(snapshot(120, 120)),
            |_| Ok(10),
            || Ok(CheckpointOutcome::Busy),
        )
        .unwrap();
        assert_eq!(report.status, MaintenanceStatus::CheckpointBusy);
        assert_eq!(report.deleted_rows, 10);
    }

    #[test]
    fn converges_after_deleting_oldest_rows() {
        let mut measurements = [snapshot(120, 120), snapshot(120, 85)].into_iter();
        let report = enforce_size_policy::<crate::LifecycleError>(
            policy(),
            || Ok(measurements.next().unwrap()),
            |_| Ok(10),
            || Ok(CheckpointOutcome::Completed),
        )
        .unwrap();
        assert_eq!(report.status, MaintenanceStatus::TargetReached);
        assert_eq!(report.rounds, 1);
    }

    #[test]
    fn retention_cutoff_checks_overflow() {
        assert_eq!(retention_cutoff_ns(100, 0).unwrap(), 100);
        assert!(retention_cutoff_ns(u64::MAX, u64::MAX).is_err());
    }
}
