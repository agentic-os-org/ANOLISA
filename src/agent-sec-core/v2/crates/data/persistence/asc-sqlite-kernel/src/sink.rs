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
use crate::maintenance::run_sqlite_maintenance_if_due;
use crate::repository::RecordRepository;
use crate::store::SqliteStore;

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
        if !self.store.is_open() {
            return;
        }
        let _ = run_sqlite_maintenance_if_due(self.store.path(), None, Some(now), || {
            self.run_maintenance(now);
            Ok(())
        });
        self.store.close();
    }

    /// Prunes according to the retention window and truncates the WAL.
    pub fn run_maintenance(&self, now: f64) {
        let _ = self.store.with_connection(false, |conn| {
            if let Some(days) = self.max_age_days {
                self.repository.prune(conn, days, now)?;
            }
            self.repository.checkpoint(conn);
            Ok(())
        });
    }
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
