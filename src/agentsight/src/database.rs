//! Process-wide registry and maintenance ownership for SQLite databases.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Duration;

use agentsight_sqlite_lifecycle::{
    LifecycleError, MaintenanceJob, MaintenanceWorker, MaintenanceWorkerState,
};

/// Stable identity of an SQLite database known to AgentSight.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum DatabaseId {
    /// Primary audit, token, HTTP, and consumption records.
    Primary,
    /// GenAI events, resource samples, and evaluation runs.
    GenAi,
    /// Interruption events and process exits.
    Interruptions,
    /// Collected ATIF trajectories.
    Trajectories,
    /// Optimization results.
    Optimization,
    /// Security audit events and risk cases.
    SecurityAudit,
    /// Enforcement control state and history.
    Enforcement,
    /// Trajectory reuse labels and decisions.
    Reuse,
    /// Durable causal-analysis cache.
    Causal,
    /// External tokenless statistics.
    Tokenless,
}

impl DatabaseId {
    /// Complete inventory of SQLite databases visible to AgentSight.
    pub const ALL: [Self; 10] = [
        Self::Primary,
        Self::GenAi,
        Self::Interruptions,
        Self::Trajectories,
        Self::Optimization,
        Self::SecurityAudit,
        Self::Enforcement,
        Self::Reuse,
        Self::Causal,
        Self::Tokenless,
    ];

    /// Returns the stable identifier used by configuration and status APIs.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Primary => "primary",
            Self::GenAi => "genai",
            Self::Interruptions => "interruptions",
            Self::Trajectories => "trajectories",
            Self::Optimization => "optimization",
            Self::SecurityAudit => "security_audit",
            Self::Enforcement => "enforcement",
            Self::Reuse => "reuse",
            Self::Causal => "causal",
            Self::Tokenless => "tokenless",
        }
    }
}

/// Long-running process role using the registry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DatabaseRole {
    /// Linux eBPF tracing and collection process.
    Trace,
    /// Linux API and Dashboard process.
    Server,
    /// Cross-platform trajectory collection process.
    LocalTrace,
    /// Cross-platform local Dashboard process.
    LocalServer,
    /// Short-lived read-only command.
    Query,
}

/// Access granted to an entry by this process.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DatabaseAccess {
    /// This process may read and write the database.
    ReadWrite,
    /// This process may only read an existing database.
    ReadOnly,
    /// Another component owns the database and AgentSight only observes it.
    External,
}

/// Declared maintenance coverage for one physical database.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DatabaseCoverage {
    /// Every known growth source has safe automatic cleanup.
    Full,
    /// Only explicitly safe tables or records can be removed.
    Partial,
    /// Growth is bounded by row count rather than byte policy.
    RowBounded,
    /// Lifecycle belongs to another component.
    External,
}

/// Immutable registration for one physical SQLite database.
#[derive(Debug, Clone)]
pub struct DatabaseSpec {
    /// Stable database identity.
    pub id: DatabaseId,
    /// Resolved local path; status serialization must not expose it.
    pub path: PathBuf,
    /// Access granted to this process.
    pub access: DatabaseAccess,
    /// Declared automatic-maintenance coverage.
    pub coverage: DatabaseCoverage,
}

impl DatabaseSpec {
    /// Defines one database entry for this process.
    pub fn new(
        id: DatabaseId,
        path: impl Into<PathBuf>,
        access: DatabaseAccess,
        coverage: DatabaseCoverage,
    ) -> Self {
        Self {
            id,
            path: path.into(),
            access,
            coverage,
        }
    }
}

/// Schema-aware work attached to one registered physical database.
pub struct DatabaseMaintenanceJob {
    id: DatabaseId,
    path: PathBuf,
    interval: Duration,
    action: Box<dyn FnMut() -> Result<(), LifecycleError> + Send>,
}

impl DatabaseMaintenanceJob {
    /// Builds a job while keeping its business-specific cleanup inside the caller.
    pub fn new(
        id: DatabaseId,
        path: impl Into<PathBuf>,
        interval: Duration,
        action: impl FnMut() -> Result<(), LifecycleError> + Send + 'static,
    ) -> Self {
        Self {
            id,
            path: path.into(),
            interval,
            action: Box::new(action),
        }
    }
}

impl MaintenanceJob for DatabaseMaintenanceJob {
    fn id(&self) -> &str {
        self.id.as_str()
    }

    fn db_path(&self) -> &Path {
        &self.path
    }

    fn interval(&self) -> Duration {
        self.interval
    }

    fn run(&mut self) -> Result<(), LifecycleError> {
        (self.action)()
    }
}

/// Errors produced by the process-wide database registry.
#[derive(Debug, thiserror::Error)]
pub enum DatabaseManagerError {
    /// A database identity was registered more than once.
    #[error("duplicate database registration: {0}")]
    DuplicateId(&'static str),
    /// Two identities unexpectedly resolve to one physical file.
    #[error("database registrations {first} and {second} resolve to the same file")]
    DuplicatePath {
        /// First registered identity.
        first: &'static str,
        /// Conflicting identity.
        second: &'static str,
    },
    /// The requested identity is not part of this process role.
    #[error("database is not registered for this process: {0}")]
    NotRegistered(&'static str),
    /// External databases cannot be opened through a managed entry.
    #[error("external database cannot be opened through the managed entry: {0}")]
    External(&'static str),
    /// The requested open mode disagrees with the registered process access.
    #[error("database {id} access mismatch: expected {expected:?}, registered {actual:?}")]
    AccessMismatch {
        /// Stable database identity.
        id: &'static str,
        /// Access requested by the caller.
        expected: DatabaseAccess,
        /// Access registered for this process.
        actual: DatabaseAccess,
    },
    /// A typed store failed to open.
    #[error("failed to open {id}: {message}")]
    StoreOpen {
        /// Stable database identity.
        id: &'static str,
        /// Store-specific failure text.
        message: String,
    },
    /// A maintenance job does not match its registered database.
    #[error("invalid maintenance registration for {0}")]
    InvalidMaintenanceJob(String),
    /// Maintenance was started more than once.
    #[error("SQLite maintenance worker is already running")]
    WorkerAlreadyRunning,
    /// The worker lock was poisoned.
    #[error("SQLite maintenance worker lock is poisoned")]
    WorkerPoisoned,
    /// The lifecycle worker failed.
    #[error(transparent)]
    Lifecycle(#[from] LifecycleError),
    /// The current directory could not be resolved.
    #[error("failed to resolve database path: {0}")]
    CurrentDirectory(#[from] std::io::Error),
}

/// Process-wide inventory and owner of the single SQLite maintenance thread.
pub struct DatabaseManager {
    role: DatabaseRole,
    entries: BTreeMap<DatabaseId, DatabaseSpec>,
    worker: Mutex<Option<MaintenanceWorker>>,
}

impl DatabaseManager {
    /// Creates a registry and rejects duplicate identities or physical paths.
    ///
    /// # Errors
    ///
    /// Returns an error when registrations are ambiguous or paths cannot be resolved.
    pub fn new(
        role: DatabaseRole,
        specs: impl IntoIterator<Item = DatabaseSpec>,
    ) -> Result<Self, DatabaseManagerError> {
        let mut entries = BTreeMap::new();
        let mut paths = HashMap::new();
        #[cfg(unix)]
        let mut inodes = HashMap::new();
        for mut spec in specs {
            spec.path = absolute_path(&spec.path)?;
            if entries.contains_key(&spec.id) {
                return Err(DatabaseManagerError::DuplicateId(spec.id.as_str()));
            }
            let identity_path = physical_identity_path(&spec.path)?;
            if let Some(previous) = paths.insert(identity_path, spec.id) {
                return Err(DatabaseManagerError::DuplicatePath {
                    first: previous.as_str(),
                    second: spec.id.as_str(),
                });
            }
            #[cfg(unix)]
            if spec.path.is_file() {
                use std::os::unix::fs::MetadataExt;

                let metadata = std::fs::metadata(&spec.path)?;
                if let Some(previous) = inodes.insert((metadata.dev(), metadata.ino()), spec.id) {
                    return Err(DatabaseManagerError::DuplicatePath {
                        first: previous.as_str(),
                        second: spec.id.as_str(),
                    });
                }
            }
            entries.insert(spec.id, spec);
        }
        Ok(Self {
            role,
            entries,
            worker: Mutex::new(None),
        })
    }

    /// Returns the process role owning this registry.
    pub const fn role(&self) -> DatabaseRole {
        self.role
    }

    /// Returns one registration without exposing it through an HTTP DTO.
    pub fn spec(&self, id: DatabaseId) -> Option<&DatabaseSpec> {
        self.entries.get(&id)
    }

    /// Opens one short-lived read-only Store through a query-role registry.
    ///
    /// # Errors
    ///
    /// Returns an error when path resolution or the typed Store open fails.
    pub fn open_query<T, E>(
        id: DatabaseId,
        path: impl Into<PathBuf>,
        coverage: DatabaseCoverage,
        opener: impl FnOnce(&Path) -> Result<T, E>,
    ) -> Result<T, DatabaseManagerError>
    where
        E: std::fmt::Display,
    {
        let manager = Self::new(
            DatabaseRole::Query,
            [DatabaseSpec::new(
                id,
                path,
                DatabaseAccess::ReadOnly,
                coverage,
            )],
        )?;
        manager.open_read_only(id, opener)
    }

    /// Opens a writable typed Store through its registered path.
    ///
    /// # Errors
    ///
    /// Returns an error when the identity is absent, not writable, or the Store rejects the path.
    pub fn open_read_write<T, E>(
        &self,
        id: DatabaseId,
        opener: impl FnOnce(&Path) -> Result<T, E>,
    ) -> Result<T, DatabaseManagerError>
    where
        E: std::fmt::Display,
    {
        self.open_with_access(id, DatabaseAccess::ReadWrite, opener)
    }

    /// Opens a read-only typed Store through its registered path.
    ///
    /// # Errors
    ///
    /// Returns an error when the identity is absent, not read-only, or the Store rejects the path.
    pub fn open_read_only<T, E>(
        &self,
        id: DatabaseId,
        opener: impl FnOnce(&Path) -> Result<T, E>,
    ) -> Result<T, DatabaseManagerError>
    where
        E: std::fmt::Display,
    {
        self.open_with_access(id, DatabaseAccess::ReadOnly, opener)
    }

    fn open_with_access<T, E>(
        &self,
        id: DatabaseId,
        expected: DatabaseAccess,
        opener: impl FnOnce(&Path) -> Result<T, E>,
    ) -> Result<T, DatabaseManagerError>
    where
        E: std::fmt::Display,
    {
        let spec = self
            .entries
            .get(&id)
            .ok_or(DatabaseManagerError::NotRegistered(id.as_str()))?;
        if spec.access == DatabaseAccess::External {
            return Err(DatabaseManagerError::External(id.as_str()));
        }
        if spec.access != expected {
            return Err(DatabaseManagerError::AccessMismatch {
                id: id.as_str(),
                expected,
                actual: spec.access,
            });
        }
        opener(&spec.path).map_err(|error| DatabaseManagerError::StoreOpen {
            id: id.as_str(),
            message: error.to_string(),
        })
    }

    /// Builds a maintenance job bound to a registered writable database.
    ///
    /// # Errors
    ///
    /// Returns an error when the database is absent or not writable in this process.
    pub fn maintenance_job(
        &self,
        id: DatabaseId,
        interval: Duration,
        action: impl FnMut() -> Result<(), LifecycleError> + Send + 'static,
    ) -> Result<Box<dyn MaintenanceJob>, DatabaseManagerError> {
        let spec = self
            .entries
            .get(&id)
            .ok_or(DatabaseManagerError::NotRegistered(id.as_str()))?;
        if spec.access != DatabaseAccess::ReadWrite {
            return Err(DatabaseManagerError::InvalidMaintenanceJob(
                id.as_str().to_owned(),
            ));
        }
        Ok(Box::new(DatabaseMaintenanceJob::new(
            id,
            spec.path.clone(),
            interval,
            action,
        )))
    }

    /// Starts the registry's single maintenance thread.
    ///
    /// Empty job sets are accepted for query-only processes and do not create a thread.
    ///
    /// # Errors
    ///
    /// Returns an error for duplicate startup, an unknown job, a path mismatch, or worker failure.
    pub fn start_maintenance(
        &self,
        jobs: Vec<Box<dyn MaintenanceJob>>,
    ) -> Result<(), DatabaseManagerError> {
        let mut worker = self
            .worker
            .lock()
            .map_err(|_| DatabaseManagerError::WorkerPoisoned)?;
        if worker.is_some() {
            return Err(DatabaseManagerError::WorkerAlreadyRunning);
        }
        if jobs.is_empty() {
            return Ok(());
        }
        for job in &jobs {
            let Some(spec) = self
                .entries
                .values()
                .find(|entry| entry.id.as_str() == job.id())
            else {
                return Err(DatabaseManagerError::InvalidMaintenanceJob(
                    job.id().to_owned(),
                ));
            };
            if spec.access != DatabaseAccess::ReadWrite
                || absolute_path(job.db_path())? != spec.path
            {
                return Err(DatabaseManagerError::InvalidMaintenanceJob(
                    job.id().to_owned(),
                ));
            }
        }
        *worker = Some(MaintenanceWorker::start(jobs)?);
        Ok(())
    }

    /// Returns the current worker snapshot, if this role runs maintenance.
    ///
    /// # Errors
    ///
    /// Returns an error when worker state cannot be read.
    pub fn maintenance_state(
        &self,
    ) -> Result<Option<MaintenanceWorkerState>, DatabaseManagerError> {
        let worker = self
            .worker
            .lock()
            .map_err(|_| DatabaseManagerError::WorkerPoisoned)?;
        worker
            .as_ref()
            .map(MaintenanceWorker::state)
            .transpose()
            .map_err(Into::into)
    }

    /// Stops and joins the maintenance thread.
    ///
    /// # Errors
    ///
    /// Returns an error if the worker or registry lock was poisoned.
    pub fn stop_maintenance(&self) -> Result<(), DatabaseManagerError> {
        let mut worker = self
            .worker
            .lock()
            .map_err(|_| DatabaseManagerError::WorkerPoisoned)?;
        if let Some(mut active) = worker.take() {
            active.join()?;
        }
        Ok(())
    }
}

impl Drop for DatabaseManager {
    fn drop(&mut self) {
        let _ = self.stop_maintenance();
    }
}

fn absolute_path(path: &Path) -> Result<PathBuf, std::io::Error> {
    std::path::absolute(path)
}

fn physical_identity_path(path: &Path) -> Result<PathBuf, std::io::Error> {
    if path.exists() {
        return std::fs::canonicalize(path);
    }
    match (path.parent(), path.file_name()) {
        (Some(parent), Some(file_name)) if parent.exists() => {
            Ok(std::fs::canonicalize(parent)?.join(file_name))
        }
        _ => Ok(path.to_path_buf()),
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    use super::*;

    fn primary(path: PathBuf) -> DatabaseSpec {
        DatabaseSpec {
            id: DatabaseId::Primary,
            path,
            access: DatabaseAccess::ReadWrite,
            coverage: DatabaseCoverage::Full,
        }
    }

    fn test_path(name: &str) -> PathBuf {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system time should follow Unix epoch")
            .as_nanos();
        std::env::temp_dir().join(format!(
            "agentsight-registry-{name}-{}-{nonce}.db",
            std::process::id()
        ))
    }

    #[test]
    fn database_inventory_has_unique_stable_ids() {
        let ids = DatabaseId::ALL.map(DatabaseId::as_str);
        let unique = ids.into_iter().collect::<std::collections::HashSet<_>>();
        assert_eq!(unique.len(), DatabaseId::ALL.len());
    }

    #[test]
    fn duplicate_database_ids_are_rejected() {
        let error = DatabaseManager::new(
            DatabaseRole::Trace,
            [primary(test_path("first")), primary(test_path("second"))],
        );
        assert!(matches!(
            error,
            Err(DatabaseManagerError::DuplicateId("primary"))
        ));
    }

    #[test]
    fn duplicate_database_paths_are_rejected() {
        let path = test_path("duplicate");
        let error = DatabaseManager::new(
            DatabaseRole::Trace,
            [
                primary(path.clone()),
                DatabaseSpec {
                    id: DatabaseId::GenAi,
                    path,
                    access: DatabaseAccess::ReadWrite,
                    coverage: DatabaseCoverage::Full,
                },
            ],
        );
        assert!(matches!(
            error,
            Err(DatabaseManagerError::DuplicatePath { .. })
        ));
    }

    #[cfg(unix)]
    #[test]
    fn symlinked_database_paths_are_rejected_without_rewriting_registered_path() {
        use std::os::unix::fs::symlink;

        let first = test_path("symlink-first");
        let second = test_path("symlink-second");
        std::fs::write(&first, []).unwrap();
        symlink(&first, &second).unwrap();
        let error = DatabaseManager::new(
            DatabaseRole::Trace,
            [
                primary(first.clone()),
                DatabaseSpec::new(
                    DatabaseId::GenAi,
                    second.clone(),
                    DatabaseAccess::ReadWrite,
                    DatabaseCoverage::Full,
                ),
            ],
        );
        assert!(matches!(
            error,
            Err(DatabaseManagerError::DuplicatePath { .. })
        ));
        std::fs::remove_file(second).unwrap();
        std::fs::remove_file(first).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn hardlinked_database_paths_are_rejected() {
        let first = test_path("hardlink-first");
        let second = test_path("hardlink-second");
        std::fs::write(&first, []).unwrap();
        std::fs::hard_link(&first, &second).unwrap();

        let error = DatabaseManager::new(
            DatabaseRole::Trace,
            [
                primary(first.clone()),
                DatabaseSpec::new(
                    DatabaseId::GenAi,
                    second.clone(),
                    DatabaseAccess::ReadWrite,
                    DatabaseCoverage::Full,
                ),
            ],
        );

        assert!(matches!(
            error,
            Err(DatabaseManagerError::DuplicatePath { .. })
        ));
        std::fs::remove_file(first).unwrap();
        std::fs::remove_file(second).unwrap();
    }

    #[test]
    fn managed_and_external_aliases_are_rejected() {
        let path = test_path("external-alias");
        let error = DatabaseManager::new(
            DatabaseRole::Server,
            [
                primary(path.clone()),
                DatabaseSpec::new(
                    DatabaseId::Tokenless,
                    path,
                    DatabaseAccess::External,
                    DatabaseCoverage::External,
                ),
            ],
        );
        assert!(matches!(
            error,
            Err(DatabaseManagerError::DuplicatePath { .. })
        ));
    }

    #[test]
    fn typed_store_open_uses_registered_path() {
        let path = test_path("open");
        let manager = DatabaseManager::new(DatabaseRole::Trace, [primary(path.clone())]).unwrap();
        let opened = manager
            .open_read_write(DatabaseId::Primary, |registered| {
                Ok::<_, std::convert::Infallible>(registered.to_path_buf())
            })
            .unwrap();
        assert_eq!(opened, absolute_path(&path).unwrap());
    }

    #[test]
    fn registered_access_mode_is_enforced() {
        let path = test_path("read-only");
        let manager = DatabaseManager::new(
            DatabaseRole::Query,
            [DatabaseSpec::new(
                DatabaseId::Primary,
                path,
                DatabaseAccess::ReadOnly,
                DatabaseCoverage::Full,
            )],
        )
        .unwrap();

        let result = manager.open_read_write(DatabaseId::Primary, |_| {
            Ok::<_, std::convert::Infallible>(())
        });
        assert!(matches!(
            result,
            Err(DatabaseManagerError::AccessMismatch { .. })
        ));
    }

    #[test]
    fn query_open_and_external_access_follow_registered_modes() {
        let query_path = test_path("query");
        let opened = DatabaseManager::open_query(
            DatabaseId::Primary,
            &query_path,
            DatabaseCoverage::Full,
            |registered| Ok::<_, std::convert::Infallible>(registered.to_path_buf()),
        )
        .unwrap();
        assert_eq!(opened, absolute_path(&query_path).unwrap());

        let manager = DatabaseManager::new(
            DatabaseRole::Server,
            [DatabaseSpec::new(
                DatabaseId::Tokenless,
                test_path("external"),
                DatabaseAccess::External,
                DatabaseCoverage::External,
            )],
        )
        .unwrap();
        assert!(matches!(
            manager.open_read_only(DatabaseId::Tokenless, |_| {
                Ok::<_, std::convert::Infallible>(())
            }),
            Err(DatabaseManagerError::External("tokenless"))
        ));
    }

    #[test]
    fn empty_and_invalid_maintenance_sets_do_not_start_a_worker() {
        let path = test_path("empty-worker");
        let manager = DatabaseManager::new(DatabaseRole::Trace, [primary(path.clone())]).unwrap();
        manager.start_maintenance(Vec::new()).unwrap();
        assert!(manager.maintenance_state().unwrap().is_none());

        let invalid = Box::new(DatabaseMaintenanceJob::new(
            DatabaseId::GenAi,
            path,
            Duration::from_secs(1),
            || Ok(()),
        ));
        assert!(matches!(
            manager.start_maintenance(vec![invalid]),
            Err(DatabaseManagerError::InvalidMaintenanceJob(_))
        ));
        manager.stop_maintenance().unwrap();
    }

    #[test]
    fn one_worker_runs_registered_job() {
        let path = test_path("worker");
        let manager = DatabaseManager::new(DatabaseRole::Trace, [primary(path.clone())]).unwrap();
        let runs = Arc::new(AtomicUsize::new(0));
        let job_runs = Arc::clone(&runs);
        let job = manager
            .maintenance_job(DatabaseId::Primary, Duration::from_secs(60), move || {
                job_runs.fetch_add(1, Ordering::SeqCst);
                Ok(())
            })
            .unwrap();
        manager.start_maintenance(vec![job]).unwrap();
        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        while runs.load(Ordering::SeqCst) == 0 {
            assert!(std::time::Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(5));
        }
        let duplicate = manager
            .maintenance_job(DatabaseId::Primary, Duration::from_secs(60), || Ok(()))
            .unwrap();
        assert!(matches!(
            manager.start_maintenance(vec![duplicate]),
            Err(DatabaseManagerError::WorkerAlreadyRunning)
        ));
        assert!(manager.maintenance_state().unwrap().is_some());
        manager.stop_maintenance().unwrap();
        assert_eq!(runs.load(Ordering::SeqCst), 1);
        let mut lock_path = path.as_os_str().to_os_string();
        lock_path.push(".maintenance.lock");
        std::fs::remove_file(PathBuf::from(lock_path)).unwrap();
    }
}
