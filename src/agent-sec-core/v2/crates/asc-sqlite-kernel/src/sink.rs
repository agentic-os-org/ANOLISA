//! The single copy of v1's eight-step write ladder.
//!
//! v1 has this decision tree twice; the copies agree on every *classification*
//! step and differ only in the terminal action, which is why the tree lives here
//! and the terminal action is delegated to a [`FaultPolicy`].
//!
//! Step order, taken from `security_events/sqlite_writer.py::write` and
//! `observability/sqlite_writer.py::write_or_raise`:
//!
//! 1. store disabled
//! 2. insert reported nothing written
//! 3. busy / locked
//! 4. schema drift — request a repair, then report
//! 5. any other database error
//! 6. corruption — clean up, then retry once
//! 7. filesystem / driver fault
//! 8. malformed record

use std::sync::{Arc, Mutex, PoisonError};

use crate::error::{KernelError, is_busy, is_corruption, is_schema};
use crate::fault::{Failure, Fault, FaultPolicy, Phase, WriteFault};
use crate::maintenance::{run_sqlite_maintenance_detailed, run_sqlite_maintenance_if_due};
use crate::repository::RecordRepository;
use crate::store::SqliteStore;

/// The least remaining stop budget the gated final maintenance pass needs
/// before it is worth starting; below this the pass is skipped and the next
/// daemon start's catch-up re-runs it (see
/// [`SqliteSink::close_with_deadline`] and
/// `DAEMON_PROCESS_DEPLOYMENT_CONTRACT_zh.md`).
const FINAL_PASS_MIN_BUDGET: std::time::Duration = std::time::Duration::from_secs(2);

/// A write pipeline over one store, one repository and one policy.
pub struct SqliteSink<R: RecordRepository, P: FaultPolicy<Record = R::Record>> {
    store: Arc<SqliteStore>,
    repository: R,
    policy: P,
    max_age_days: Option<u32>,
    /// Present only for streams that serialize writes.
    ///
    /// v1 `security_events` holds a `threading.Lock`; `observability` holds none.
    write_lock: Option<Mutex<()>>,
}

impl<R: RecordRepository, P: FaultPolicy<Record = R::Record>> SqliteSink<R, P> {
    /// Builds a sink.
    ///
    /// `serialize_writes` mirrors v1's per-stream write lock, and `max_age_days`
    /// is the retention window applied by [`SqliteSink::close`].
    pub fn new(
        store: Arc<SqliteStore>,
        repository: R,
        policy: P,
        max_age_days: Option<u32>,
        serialize_writes: bool,
    ) -> Self {
        Self {
            store,
            repository,
            policy,
            max_age_days,
            write_lock: serialize_writes.then(|| Mutex::new(())),
        }
    }

    /// Returns the underlying store.
    #[must_use]
    pub fn store(&self) -> &Arc<SqliteStore> {
        &self.store
    }

    /// Returns the repository.
    #[must_use]
    pub const fn repository(&self) -> &R {
        &self.repository
    }

    /// Returns the fault policy.
    #[must_use]
    pub const fn policy(&self) -> &P {
        &self.policy
    }

    /// Returns the retention window in days, if any.
    #[must_use]
    pub const fn max_age_days(&self) -> Option<u32> {
        self.max_age_days
    }

    /// Returns whether writes are serialized by this sink.
    #[must_use]
    pub const fn serializes_writes(&self) -> bool {
        self.write_lock.is_some()
    }

    /// Persists `record`, swallowing every fault.
    pub fn write(&self, record: &R::Record) {
        let _ = self.persist(record);
    }

    /// Persists `record`, surfacing faults the policy asks to surface.
    ///
    /// # Errors
    ///
    /// Returns whatever the [`FaultPolicy`] turns into a [`Failure`].
    pub fn write_or_raise(&self, record: &R::Record) -> Result<(), KernelError> {
        self.persist(record)
    }

    fn persist(&self, record: &R::Record) -> Result<(), KernelError> {
        let _guard = self
            .write_lock
            .as_ref()
            .map(|lock| lock.lock().unwrap_or_else(PoisonError::into_inner));

        match self.classify(record) {
            None => Ok(()),
            Some(fault) => self.apply(fault, record),
        }
    }

    /// Runs the ladder and returns the fault, or `None` on success.
    fn classify(&self, record: &R::Record) -> Option<OwnedFault> {
        // Step 1.
        if self.store.is_disabled() {
            return Some(OwnedFault::new(
                WriteFault::Disabled,
                Phase::Insert,
                false,
                None,
            ));
        }

        let attempt = self
            .store
            .with_connection(true, |conn| self.repository.insert_or_raise(conn, record));

        let error = match attempt {
            // Step 2: a live connection that wrote nothing, or no connection at
            // all, both mean "skipped" in v1.
            Ok(Some(true)) => return None,
            Ok(Some(false) | None) => {
                return Some(OwnedFault::new(
                    WriteFault::Skipped,
                    Phase::Insert,
                    false,
                    None,
                ));
            }
            Err(err) => err,
        };

        // Step 8: a malformed record never touched SQLite.
        if matches!(error, KernelError::Malformed(_)) {
            return Some(OwnedFault::new(
                WriteFault::Malformed,
                Phase::Insert,
                false,
                Some(error),
            ));
        }

        // Step 7: filesystem / driver faults.
        if matches!(error, KernelError::Io { .. }) {
            return Some(OwnedFault::new(
                WriteFault::Io,
                Phase::Io,
                false,
                Some(error),
            ));
        }

        if matches!(error, KernelError::Disabled) {
            return Some(OwnedFault::new(
                WriteFault::Disabled,
                Phase::Insert,
                false,
                Some(error),
            ));
        }

        // Step 3.
        if is_busy(&error) {
            return Some(OwnedFault::new(
                WriteFault::Busy,
                Phase::Insert,
                true,
                Some(error),
            ));
        }

        // Steps 4 and 5: anything that is not corruption stops here, and schema
        // drift additionally schedules a repair.
        if !is_corruption(&error) {
            if is_schema(&error) {
                self.store.request_schema_repair();
                return Some(OwnedFault::new(
                    WriteFault::Schema,
                    Phase::Insert,
                    false,
                    Some(error),
                ));
            }
            return Some(OwnedFault::new(
                WriteFault::Database,
                Phase::Insert,
                false,
                Some(error),
            ));
        }

        // Step 6: corruption cleanup, then one retry.
        self.store.handle_corruption(&error);
        if self.store.is_disabled() {
            return Some(OwnedFault::new(
                WriteFault::Corruption,
                Phase::CorruptionDisabled,
                false,
                Some(error),
            ));
        }
        self.retry_after_corruption(record)
    }

    /// Replays the insert once after a corruption rebuild.
    ///
    /// Every fault from here carries [`Phase::CorruptionRetry`], and the `busy`
    /// flag is what lets a policy reproduce v1's asymmetric dispose decision.
    fn retry_after_corruption(&self, record: &R::Record) -> Option<OwnedFault> {
        match self
            .store
            .with_connection(true, |conn| self.repository.insert_or_raise(conn, record))
        {
            Ok(Some(true)) => None,
            Ok(Some(false) | None) => Some(OwnedFault::new(
                WriteFault::Skipped,
                Phase::CorruptionRetry,
                false,
                None,
            )),
            Err(retry_err) => {
                let busy = is_busy(&retry_err);
                let kind = if matches!(retry_err, KernelError::Malformed(_)) {
                    WriteFault::Malformed
                } else if busy {
                    WriteFault::Busy
                } else {
                    WriteFault::Database
                };
                Some(OwnedFault::new(
                    kind,
                    Phase::CorruptionRetry,
                    busy,
                    Some(retry_err),
                ))
            }
        }
    }

    /// Asks the policy what to do and carries it out.
    fn apply(&self, fault: OwnedFault, record: &R::Record) -> Result<(), KernelError> {
        let outcome = self.policy.on_fault(
            &Fault {
                kind: fault.kind,
                phase: fault.phase,
                busy: fault.busy,
                error: fault.error.as_ref(),
            },
            record,
        );

        if outcome.dispose {
            self.store.dispose();
        }

        match outcome.failure {
            None => Ok(()),
            Some(Failure::Message(message)) => Err(KernelError::io(
                "write to",
                self.store.path(),
                std::io::Error::other(message),
            )),
            Some(Failure::Original) => Err(fault.error.unwrap_or_else(|| {
                KernelError::io(
                    "write to",
                    self.store.path(),
                    std::io::Error::other(format!("{:?} without an underlying error", fault.kind)),
                )
            })),
        }
    }

    /// Runs the gated maintenance pass and drops the connection.
    ///
    /// Like v1 `close()`, this is a no-op when the store was never opened, so a
    /// process that never wrote anything performs no maintenance.
    pub fn close(&self, now: f64) {
        self.close_with_deadline(now, None);
    }

    /// [`close`](SqliteSink::close) with an explicit wall-clock deadline for
    /// the gated maintenance pass.
    ///
    /// The daemon's shutdown path passes the instant by which the final pass
    /// must finish (the tail of the unit's `TimeoutStopSec` budget, see
    /// `DAEMON_PROCESS_DEPLOYMENT_CONTRACT_zh.md`): a periodic pass that kept
    /// failing leaves the gate marker unadvanced, so the final pass would
    /// otherwise re-run a full prune that no longer fits the stop budget.
    /// When less than [`FINAL_PASS_MIN_BUDGET`] remains, the pass is skipped
    /// and only the connection is dropped - the gate marker stays
    /// unadvanced, and the next daemon start's catch-up pass re-runs the
    /// prune instead, the same recovery a hard kill already relies on. A
    /// pass that starts inside the window but overruns it is cut off by
    /// systemd's `TimeoutStopSec`, which is that same hard-kill path.
    pub fn close_with_deadline(&self, now: f64, deadline: Option<std::time::Instant>) {
        if !self.store.is_open() {
            return;
        }
        if let Some(deadline) = deadline {
            let remaining = deadline.saturating_duration_since(std::time::Instant::now());
            if remaining < FINAL_PASS_MIN_BUDGET {
                tracing::warn!(
                    target: "asc_process_diagnostic",
                    remaining_secs = remaining.as_secs_f64(),
                    "sqlite final retention pass skipped: the stop budget cannot fit it; the next startup catch-up re-runs it"
                );
                self.store.close();
                return;
            }
        }
        let _ = run_sqlite_maintenance_if_due(self.store.path(), None, Some(now), || {
            self.run_maintenance(now)
        });
        self.store.close();
    }

    /// Runs the gated maintenance pass without closing the store, with the
    /// gate's outcome surfaced for the caller's diagnostics.
    ///
    /// The long-lived daemon composes this into its periodic retention task;
    /// [`SqliteSink::close`] remains the one-shot exit path. The structured
    /// outcome distinguishes a closed gate ([`MaintenanceOutcome::NotDue`])
    /// from a contended lock ([`MaintenanceOutcome::Contended`]) and from real
    /// failures ([`MaintenanceOutcome::Failed`]): lock-file, prune, and marker
    /// failures all report `Failed` and leave the marker unadvanced, so the
    /// next call retries instead of waiting a full window.
    #[must_use]
    pub fn run_maintenance_detailed(&self, now: f64) -> MaintenanceOutcome {
        if !self.store.is_open() {
            return MaintenanceOutcome::NotDue;
        }
        run_sqlite_maintenance_detailed(self.store.path(), None, Some(now), || {
            self.run_maintenance(now)
        })
    }

    /// Prunes according to the retention window and truncates the WAL.
    ///
    /// Errors propagate so the maintenance gate does not advance its marker
    /// over a failed pass - a swallowed error here used to make the next
    /// retry wait a full window while the expired rows stayed in place.
    ///
    /// # Errors
    ///
    /// Returns [`KernelError`] when the prune fails, the connection cannot
    /// be taken, or the store is permanently disabled.
    pub fn run_maintenance(&self, now: f64) -> Result<(), KernelError> {
        self.store
            .with_connection(false, |conn| {
                if let Some(days) = self.max_age_days {
                    self.repository.prune(conn, days, now)?;
                }
                self.repository.checkpoint(conn);
                Ok(())
            })?
            .ok_or(KernelError::Disabled)
    }
}

/// The outcome of one gated maintenance attempt.
///
/// The gate's own failures are surfaced rather than folded into `NotDue`:
/// a lock held by another process is [`MaintenanceOutcome::Contended`], and
/// lock-file, prune, or marker failures are [`MaintenanceOutcome::Failed`].
/// None of them advance the gate's marker, so for a caller the practical
/// difference is only in diagnostics and retry policy - `NotDue` returns to
/// the regular cadence, the others retry sooner.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MaintenanceOutcome {
    /// The gate was closed; nothing ran.
    NotDue,
    /// Another process held the maintenance lock; nothing ran.
    Contended,
    /// The pass ran and the marker advanced.
    Ran,
    /// The pass or the gate itself failed; the marker was not advanced, so the
    /// next attempt retries rather than waiting a full window.
    Failed(String),
}

impl<R: RecordRepository, P: FaultPolicy<Record = R::Record>> std::fmt::Debug for SqliteSink<R, P> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SqliteSink")
            .field("store", &self.store)
            .field("max_age_days", &self.max_age_days)
            .field("serializes_writes", &self.serializes_writes())
            .finish_non_exhaustive()
    }
}

/// A [`Fault`] that owns its error, so the ladder can return it.
struct OwnedFault {
    kind: WriteFault,
    phase: Phase,
    busy: bool,
    error: Option<KernelError>,
}

impl OwnedFault {
    const fn new(kind: WriteFault, phase: Phase, busy: bool, error: Option<KernelError>) -> Self {
        Self {
            kind,
            phase,
            busy,
            error,
        }
    }
}
