//! Authoritative local Policy state. No automatic repair, fallback, or remote I/O.
#![forbid(unsafe_code)]
mod migration;
mod pap;
mod path;
mod reconciliation;
mod records;
mod table;
pub use path::DatabaseLease;

use asc_pap::PapError;
use asc_policy_repository::StoreError;
use rusqlite::{Connection, Transaction, TransactionBehavior};
use std::path::Path;
use std::sync::{Arc, Mutex};

/// Safe startup and storage failure categories; SQL values never enter diagnostics.
#[derive(Debug, thiserror::Error)]
pub enum RepositoryError {
    #[error("unsafe Policy data path or permissions (directory 0700, files 0600 required)")]
    UnsafePath,
    #[error("Policy database already owned by another daemon")]
    AlreadyOpen,
    #[error("Policy file operation failed")]
    Io(#[from] std::io::Error),
    #[error("{0}")]
    Storage(#[from] StoreError),
    #[error("{0}")]
    Request(#[from] PapError),
}
impl From<rustix::io::Errno> for RepositoryError {
    fn from(value: rustix::io::Errno) -> Self {
        Self::Io(value.into())
    }
}
impl From<rusqlite::Error> for RepositoryError {
    fn from(value: rusqlite::Error) -> Self {
        use rusqlite::ErrorCode;
        if matches!(
            value,
            rusqlite::Error::FromSqlConversionFailure(..)
                | rusqlite::Error::InvalidColumnType(..)
                | rusqlite::Error::IntegralValueOutOfRange(..)
        ) {
            return Self::Storage(StoreError::Corrupt);
        }
        let kind = match value.sqlite_error_code() {
            Some(ErrorCode::DatabaseBusy | ErrorCode::DatabaseLocked) => StoreError::Busy,
            Some(ErrorCode::DiskFull) => StoreError::Full,
            Some(ErrorCode::ReadOnly) => StoreError::ReadOnly,
            Some(ErrorCode::DatabaseCorrupt | ErrorCode::NotADatabase) => StoreError::Corrupt,
            Some(ErrorCode::ConstraintViolation) => StoreError::Invalid,
            _ => StoreError::Io,
        };
        Self::Storage(kind)
    }
}
impl RepositoryError {
    fn store(self) -> StoreError {
        match self {
            Self::Storage(e) => e,
            Self::UnsafePath => StoreError::Corrupt,
            _ => StoreError::Unavailable,
        }
    }
    fn pap(self) -> PapError {
        match self {
            Self::Request(e) => e,
            Self::Storage(StoreError::Busy) => PapError::Unavailable,
            _ => PapError::Persistence,
        }
    }
}

struct Storage {
    connection: Option<Connection>,
    failure: Option<StoreError>,
}

/// One serialized connection shared by PAP, PCP and recovery scans.
pub struct SqlitePolicyRepository {
    storage: Mutex<Storage>,
    lease: Arc<DatabaseLease>,
}

impl SqlitePolicyRepository {
    /// Opens or initializes strictly; existing authoritative files are never removed.
    /// # Errors
    /// Rejects unsafe paths, another owner, incompatible schema, corruption or unavailable storage.
    pub fn open(path: &Path) -> Result<Self, RepositoryError> {
        let lease = Arc::new(DatabaseLease::acquire(path)?);
        let connection = open_connection(&lease)?;
        let repository = Self {
            storage: Mutex::new(Storage {
                connection: Some(connection),
                failure: None,
            }),
            lease,
        };
        repository.check_storage()?;
        repository.lease.sync_directory()?;
        Ok(repository)
    }

    /// Retain this lease outside the daemon's outer asynchronous-runtime drain.
    pub fn lease(&self) -> Arc<DatabaseLease> {
        self.lease.clone()
    }

    fn access<T>(
        &self,
        write: bool,
        operation: impl FnOnce(&Transaction<'_>) -> Result<T, RepositoryError>,
    ) -> Result<T, RepositoryError> {
        use std::panic::{AssertUnwindSafe, catch_unwind, resume_unwind};
        let mut storage = self.storage.lock().map_err(|_| StoreError::Unavailable)?;
        if let Some(error @ (StoreError::Corrupt | StoreError::Incompatible)) = storage.failure {
            return Err(error.into());
        }
        let result = catch_unwind(AssertUnwindSafe(|| {
            self.lease.verify()?;
            if storage.connection.is_none() {
                storage.connection = Some(open_connection(&self.lease)?);
            }
            let connection = storage.connection.as_mut().ok_or(StoreError::Unavailable)?;
            let transaction = connection.transaction_with_behavior(if write {
                TransactionBehavior::Immediate
            } else {
                TransactionBehavior::Deferred
            })?;
            let value = match operation(&transaction) {
                Ok(value) => value,
                Err(error) => {
                    if let Err(rollback) = transaction.rollback() {
                        tracing::warn!(target: "asc_process_diagnostic", "Policy transaction rollback failed; connection will be discarded");
                        return Err(if matches!(error, RepositoryError::Storage(_)) {
                            error
                        } else {
                            rollback.into()
                        });
                    }
                    return Err(error);
                }
            };
            if let Err(error) = transaction.commit() {
                let classified = RepositoryError::from(error).store();
                storage.failure = Some(classified);
                // Preserve the underlying fatal category while the caller resolves
                // its original write ID after any uncertain commit outcome.
                return Err(RepositoryError::Storage(if write {
                    StoreError::OutcomeUnknown
                } else {
                    classified
                }));
            }
            Ok(value)
        }));
        let result = match result {
            Ok(result) => result,
            Err(payload) => {
                storage.connection.take();
                storage.failure = Some(StoreError::Io);
                drop(storage);
                resume_unwind(payload)
            }
        };
        if let Err(error) = &result {
            let category = match error {
                RepositoryError::Storage(error) => Some(*error),
                RepositoryError::UnsafePath => Some(StoreError::Corrupt),
                RepositoryError::Io(_) => Some(StoreError::Io),
                _ => None,
            };
            if let Some(error) =
                category.filter(|e| !matches!(e, StoreError::Invalid | StoreError::Contended))
            {
                if !matches!(
                    storage.failure,
                    Some(StoreError::Corrupt | StoreError::Incompatible)
                ) {
                    storage.failure = Some(error);
                }
                storage.connection.take();
            }
        } else if write {
            storage.failure = None;
        }
        result
    }

    fn check_storage(&self) -> Result<(), RepositoryError> {
        self.access(true, |tx| {
            // A real rolled-back write checks the main database, including FULL/read-only.
            tx.execute_batch(
                "SAVEPOINT writable_probe;
                 INSERT INTO policies(policy_id,last_allocated_revision)
                 VALUES ('__policy_storage_probe__',1) ON CONFLICT(policy_id)
                 DO UPDATE SET last_allocated_revision=last_allocated_revision;
                 ROLLBACK TO writable_probe;
                 RELEASE writable_probe;",
            )?;
            Ok(())
        })
    }
}

fn open_connection(lease: &DatabaseLease) -> Result<Connection, RepositoryError> {
    lease.verify()?;
    let mut connection = Connection::open(&lease.path)?;
    connection.execute_batch("PRAGMA busy_timeout=200; PRAGMA journal_mode=WAL; PRAGMA synchronous=FULL; PRAGMA foreign_keys=ON;")?;
    let journal: String = connection.query_row("PRAGMA journal_mode", [], |r| r.get(0))?;
    for (pragma, expected) in [
        ("synchronous", 2),
        ("foreign_keys", 1),
        ("busy_timeout", 200),
    ] {
        let actual: i64 = connection.pragma_query_value(None, pragma, |r| r.get(0))?;
        if actual != expected {
            return Err(StoreError::Incompatible.into());
        }
    }
    if journal != "wal" {
        return Err(StoreError::Incompatible.into());
    }
    migration::migrate(&mut connection)?;
    let mut expected = Connection::open_in_memory()?;
    migration::migrate(&mut expected)?;
    let schema =
        |db: &Connection| -> Result<Vec<(String, String, Option<String>)>, rusqlite::Error> {
            db.prepare("SELECT type,name,sql FROM sqlite_schema ORDER BY type,name")?
                .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
                .collect()
        };
    if schema(&connection)? != schema(&expected)? {
        return Err(StoreError::Incompatible.into());
    }
    let check: String = connection.query_row("PRAGMA quick_check", [], |r| r.get(0))?;
    if check != "ok"
        || connection
            .prepare("PRAGMA foreign_key_check")?
            .query([])?
            .next()?
            .is_some()
    {
        return Err(StoreError::Corrupt.into());
    }
    let tx = connection.transaction()?;
    records::validate_records(&tx)?;
    tx.commit()?;
    lease.verify()?;
    Ok(connection)
}

#[cfg(test)]
mod tests;
