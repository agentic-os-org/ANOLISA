//! Read-only SQLite policy and capacity status.

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use agentsight_sqlite_lifecycle::{
    ConnectionMode, ConnectionOptions, MaintenanceJobResult, MaintenanceWorkerState, SizeSnapshot,
    measure_database, open_connection,
};
use serde::Serialize;

use crate::config::{PeriodicStoragePolicy, StorageConfig};
use crate::database::{DatabaseId, DatabaseManager};

/// Storage status response consumed by the Dashboard settings page.
#[derive(Debug, Serialize)]
pub struct StorageStatusResponse {
    /// Response schema version.
    pub schema_version: u32,
    /// Observation time in Unix milliseconds.
    pub observed_at_unix_ms: u64,
    /// Status of each known SQLite store.
    pub stores: Vec<StoreStatus>,
}

/// Read-only status for one SQLite store.
#[derive(Debug, Serialize)]
pub struct StoreStatus {
    /// Stable store identifier.
    pub id: &'static str,
    /// Whether the database can be measured.
    pub availability: Availability,
    /// Current allocation when measurable.
    pub size: Option<SizeStatus>,
    /// Effective retention and capacity policy.
    pub policy: PolicyStatus,
    /// Scope covered by automatic lifecycle maintenance.
    pub coverage: Coverage,
    /// Current relationship to the configured capacity threshold.
    pub size_state: SizeState,
    /// Runtime state of automatic lifecycle maintenance.
    pub maintenance: MaintenanceRuntimeStatus,
}

/// Runtime state for one store's lifecycle job.
#[derive(Debug, Default, Serialize)]
pub struct MaintenanceRuntimeStatus {
    /// Whether this process registered a maintenance job for the store.
    pub scheduled: bool,
    /// Whether the worker running this store's registered job is active.
    pub worker_running: bool,
    /// Unix timestamp in milliseconds for the latest scheduler heartbeat.
    pub worker_heartbeat_unix_ms: Option<u64>,
    /// Unix timestamp in milliseconds for the latest attempt.
    pub last_attempt_unix_ms: Option<u64>,
    /// Unix timestamp in milliseconds for the latest successful attempt.
    pub last_success_unix_ms: Option<u64>,
    /// Categorized result of the latest attempt.
    pub last_result: Option<MaintenanceResultStatus>,
    /// Number of unsuccessful attempts since the latest success.
    pub consecutive_failures: u64,
    /// Unix timestamp in milliseconds for the next scheduled attempt.
    pub next_run_unix_ms: Option<u64>,
}

/// Categorized result of the latest lifecycle attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MaintenanceResultStatus {
    /// The job completed successfully.
    Success,
    /// The job returned an error.
    Error,
    /// Another process held the maintenance lock.
    LockBusy,
    /// The worker contained a panic from the job.
    Panicked,
}

impl From<MaintenanceJobResult> for MaintenanceResultStatus {
    fn from(result: MaintenanceJobResult) -> Self {
        match result {
            MaintenanceJobResult::Success => Self::Success,
            MaintenanceJobResult::Error => Self::Error,
            MaintenanceJobResult::LockBusy => Self::LockBusy,
            MaintenanceJobResult::Panicked => Self::Panicked,
        }
    }
}

/// Database availability without exposing filesystem paths.
#[derive(Debug, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Availability {
    /// The database exists and was measured.
    Present,
    /// The database has not been created.
    Missing,
    /// The database exists but cannot be measured.
    Error,
    /// Another component owns the database.
    External,
}

/// Current SQLite file allocation.
#[derive(Debug, Serialize)]
pub struct SizeStatus {
    /// Main database bytes.
    pub database_bytes: u64,
    /// WAL sidecar bytes.
    pub wal_bytes: u64,
    /// Shared-memory sidecar bytes.
    pub shm_bytes: u64,
    /// Reusable freelist bytes.
    pub freelist_bytes: u64,
    /// Main, WAL, and SHM bytes combined.
    pub physical_bytes: u64,
    /// Physical bytes minus reusable freelist pages.
    pub logical_bytes: u64,
}

impl From<SizeSnapshot> for SizeStatus {
    fn from(snapshot: SizeSnapshot) -> Self {
        Self {
            database_bytes: snapshot.database_bytes,
            wal_bytes: snapshot.wal_bytes,
            shm_bytes: snapshot.shm_bytes,
            freelist_bytes: snapshot.freelist_bytes,
            physical_bytes: snapshot.physical_bytes,
            logical_bytes: snapshot.logical_bytes,
        }
    }
}

/// Effective lifecycle policy for one store.
#[derive(Debug, Serialize)]
pub struct PolicyStatus {
    /// Maximum record age; zero disables age cleanup.
    pub retention_days: u64,
    /// Maximum logical size; zero disables size cleanup.
    pub size_limit_bytes: u64,
    /// Size that starts cleanup.
    pub cleanup_trigger_bytes: u64,
    /// Size that ends cleanup.
    pub cleanup_target_bytes: u64,
    /// Automatic check interval.
    pub check_interval: u64,
    /// Unit of `check_interval`.
    pub check_interval_unit: &'static str,
    /// Process responsible for maintenance.
    pub enforced_by: &'static str,
}

/// Completeness of automatic cleanup coverage.
#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Coverage {
    /// Every growth source in the database is governed.
    Full,
    /// Only explicitly safe growth sources are governed.
    Partial,
    /// Only a bounded row count is enforced.
    RowBounded,
    /// No automatic lifecycle policy is active.
    Unmanaged,
    /// Another component owns lifecycle management.
    External,
}

/// Snapshot relationship to the configured capacity policy.
#[derive(Debug, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SizeState {
    /// Logical usage is within the configured threshold.
    WithinPolicy,
    /// Logical usage exceeds the configured threshold.
    CleanupDue,
    /// Physical allocation exceeds the limit but reusable pages keep logical usage within it.
    ReusableCapacity,
    /// Size enforcement is disabled.
    Disabled,
    /// No meaningful byte policy applies.
    Unknown,
}

/// Collects Linux storage status without changing any database.
pub fn collect_storage_status(
    genai_path: &Path,
    config: &StorageConfig,
    manager: Option<&DatabaseManager>,
) -> StorageStatusResponse {
    let private = config.base_path.join(".agentsight-private");
    let maintenance_state = manager.and_then(|manager| manager.maintenance_state().ok().flatten());
    let definitions = [
        (
            DatabaseId::Primary,
            config.primary_path(),
            config.primary,
            "trace",
            90,
            Coverage::Full,
        ),
        (
            DatabaseId::GenAi,
            genai_path.to_path_buf(),
            config.genai,
            "trace_and_serve",
            90,
            Coverage::Full,
        ),
        (
            DatabaseId::Interruptions,
            config.interruption_path(),
            config.interruptions,
            "trace_and_serve",
            90,
            Coverage::Full,
        ),
        (
            DatabaseId::Trajectories,
            config.trajectory_path(),
            config.trajectories,
            "trajectory_collector",
            90,
            Coverage::Partial,
        ),
        (
            DatabaseId::Optimization,
            config.optimization_path(),
            config.optimization,
            "serve",
            90,
            Coverage::Full,
        ),
        (
            DatabaseId::SecurityAudit,
            private.join(crate::config::SECURITY_AUDIT_DB_NAME),
            config.security_audit,
            "serve",
            90,
            Coverage::Partial,
        ),
        (
            DatabaseId::Reuse,
            private.join(crate::config::REUSE_DB_NAME),
            config.reuse,
            "serve",
            90,
            Coverage::Partial,
        ),
        (
            DatabaseId::Causal,
            private.join(crate::config::CAUSAL_DB_NAME),
            config.causal,
            "serve",
            90,
            Coverage::Partial,
        ),
        (
            DatabaseId::Enforcement,
            private.join(crate::config::ENFORCEMENT_DB_NAME),
            config.enforcement,
            "serve",
            90,
            Coverage::Partial,
        ),
    ];
    let mut stores = definitions
        .into_iter()
        .map(
            |(id, fallback, policy, enforced_by, target_percent, coverage)| {
                measured_periodic_store(
                    id,
                    managed_path(manager, id, fallback),
                    policy,
                    enforced_by,
                    target_percent,
                    managed_coverage(manager, id, coverage),
                    maintenance_state.as_ref(),
                )
            },
        )
        .collect::<Vec<_>>();
    stores.push(StoreStatus {
        id: DatabaseId::Tokenless.as_str(),
        availability: Availability::External,
        size: None,
        policy: PolicyStatus {
            retention_days: 0,
            size_limit_bytes: 0,
            cleanup_trigger_bytes: 0,
            cleanup_target_bytes: 0,
            check_interval: 0,
            check_interval_unit: "external",
            enforced_by: "tokenless",
        },
        coverage: Coverage::External,
        size_state: SizeState::Unknown,
        maintenance: maintenance_status(DatabaseId::Tokenless, maintenance_state.as_ref()),
    });

    response(stores)
}

/// Collects the subset available in local trajectory-viewer mode.
pub fn collect_local_storage_status(
    trajectory_path: &Path,
    config: &StorageConfig,
    manager: Option<&DatabaseManager>,
) -> StorageStatusResponse {
    let maintenance_state = manager.and_then(|manager| manager.maintenance_state().ok().flatten());
    let reuse_path = config
        .base_path
        .join(".agentsight-private")
        .join(crate::config::REUSE_DB_NAME);
    let mut stores = vec![
        measured_periodic_store(
            DatabaseId::Trajectories,
            managed_path(
                manager,
                DatabaseId::Trajectories,
                trajectory_path.to_path_buf(),
            ),
            config.trajectories,
            "local_trace",
            90,
            managed_coverage(manager, DatabaseId::Trajectories, Coverage::Partial),
            maintenance_state.as_ref(),
        ),
        measured_periodic_store(
            DatabaseId::Optimization,
            managed_path(
                manager,
                DatabaseId::Optimization,
                config.optimization_path(),
            ),
            config.optimization,
            "local_server",
            90,
            managed_coverage(manager, DatabaseId::Optimization, Coverage::Full),
            maintenance_state.as_ref(),
        ),
        measured_periodic_store(
            DatabaseId::Reuse,
            managed_path(manager, DatabaseId::Reuse, reuse_path),
            config.reuse,
            "local_server",
            90,
            managed_coverage(manager, DatabaseId::Reuse, Coverage::Partial),
            maintenance_state.as_ref(),
        ),
    ];
    stores.push(StoreStatus {
        id: DatabaseId::Tokenless.as_str(),
        availability: Availability::External,
        size: None,
        policy: PolicyStatus {
            retention_days: 0,
            size_limit_bytes: 0,
            cleanup_trigger_bytes: 0,
            cleanup_target_bytes: 0,
            check_interval: 0,
            check_interval_unit: "external",
            enforced_by: "tokenless",
        },
        coverage: Coverage::External,
        size_state: SizeState::Unknown,
        maintenance: maintenance_status(DatabaseId::Tokenless, maintenance_state.as_ref()),
    });
    response(stores)
}

fn response(stores: Vec<StoreStatus>) -> StorageStatusResponse {
    StorageStatusResponse {
        schema_version: 2,
        observed_at_unix_ms: SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|duration| u64::try_from(duration.as_millis()).unwrap_or(u64::MAX))
            .unwrap_or(0),
        stores,
    }
}

fn managed_path(manager: Option<&DatabaseManager>, id: DatabaseId, fallback: PathBuf) -> PathBuf {
    manager
        .and_then(|manager| manager.spec(id))
        .map(|spec| spec.path.clone())
        .unwrap_or(fallback)
}

fn managed_coverage(
    manager: Option<&DatabaseManager>,
    id: DatabaseId,
    fallback: Coverage,
) -> Coverage {
    manager
        .and_then(|manager| manager.spec(id))
        .map(|spec| match spec.coverage {
            crate::database::DatabaseCoverage::Full => Coverage::Full,
            crate::database::DatabaseCoverage::Partial => Coverage::Partial,
            crate::database::DatabaseCoverage::RowBounded => Coverage::RowBounded,
            crate::database::DatabaseCoverage::External => Coverage::External,
        })
        .unwrap_or(fallback)
}

fn database_coverage(coverage: Coverage) -> crate::database::DatabaseCoverage {
    match coverage {
        Coverage::Full => crate::database::DatabaseCoverage::Full,
        Coverage::Partial => crate::database::DatabaseCoverage::Partial,
        Coverage::RowBounded => crate::database::DatabaseCoverage::RowBounded,
        Coverage::Unmanaged | Coverage::External => crate::database::DatabaseCoverage::External,
    }
}

fn measured_periodic_store(
    id: DatabaseId,
    path: PathBuf,
    policy: PeriodicStoragePolicy,
    enforced_by: &'static str,
    target_percent: u64,
    coverage: Coverage,
    maintenance_state: Option<&MaintenanceWorkerState>,
) -> StoreStatus {
    measured_store(
        id,
        path,
        policy_status(
            policy.retention_days,
            policy.max_db_size_mb,
            policy.check_interval_secs,
            "seconds",
            enforced_by,
            target_percent,
        ),
        coverage,
        maintenance_state,
    )
}

fn policy_status(
    retention_days: u64,
    max_db_size_mb: u64,
    check_interval: u64,
    check_interval_unit: &'static str,
    enforced_by: &'static str,
    target_percent: u64,
) -> PolicyStatus {
    let limit = max_db_size_mb.saturating_mul(1024 * 1024);
    PolicyStatus {
        retention_days,
        size_limit_bytes: limit,
        cleanup_trigger_bytes: limit,
        cleanup_target_bytes: limit.saturating_mul(target_percent) / 100,
        check_interval,
        check_interval_unit,
        enforced_by,
    }
}

fn measured_store(
    id: DatabaseId,
    path: PathBuf,
    policy: PolicyStatus,
    coverage: Coverage,
    maintenance_state: Option<&MaintenanceWorkerState>,
) -> StoreStatus {
    let maintenance = maintenance_status(id, maintenance_state);
    if !path.is_file() {
        return StoreStatus {
            id: id.as_str(),
            availability: Availability::Missing,
            size: None,
            size_state: state_without_measurement(&policy, &coverage),
            policy,
            coverage,
            maintenance,
        };
    }

    let options = ConnectionOptions {
        mode: ConnectionMode::ReadOnlyExisting,
        enable_wal: false,
        ..ConnectionOptions::default()
    };
    let snapshot =
        DatabaseManager::open_query(id, &path, database_coverage(coverage), |registered| {
            let connection = open_connection(registered, options)?;
            measure_database(registered, &connection)
        });
    match snapshot {
        Ok(snapshot) => {
            let size_state = if matches!(coverage, Coverage::Full | Coverage::Partial) {
                classify_size(&policy, snapshot)
            } else {
                SizeState::Unknown
            };
            StoreStatus {
                id: id.as_str(),
                availability: Availability::Present,
                size: Some(snapshot.into()),
                policy,
                coverage,
                size_state,
                maintenance,
            }
        }
        Err(_) => StoreStatus {
            id: id.as_str(),
            availability: Availability::Error,
            size: None,
            size_state: SizeState::Unknown,
            policy,
            coverage,
            maintenance,
        },
    }
}

fn maintenance_status(
    id: DatabaseId,
    worker_state: Option<&MaintenanceWorkerState>,
) -> MaintenanceRuntimeStatus {
    let Some(worker_state) = worker_state else {
        return MaintenanceRuntimeStatus::default();
    };
    let Some(job) = worker_state.jobs.iter().find(|job| job.id == id.as_str()) else {
        return MaintenanceRuntimeStatus::default();
    };

    MaintenanceRuntimeStatus {
        scheduled: true,
        worker_running: worker_state.running,
        worker_heartbeat_unix_ms: Some(worker_state.heartbeat_unix_ms),
        last_attempt_unix_ms: job.last_attempt_unix_ms,
        last_success_unix_ms: job.last_success_unix_ms,
        last_result: job.last_result.map(Into::into),
        consecutive_failures: job.consecutive_failures,
        next_run_unix_ms: Some(job.next_run_unix_ms),
    }
}

fn classify_size(policy: &PolicyStatus, snapshot: SizeSnapshot) -> SizeState {
    if policy.size_limit_bytes == 0 {
        return SizeState::Disabled;
    }
    if snapshot.physical_bytes > policy.cleanup_trigger_bytes
        && snapshot.logical_bytes > policy.cleanup_target_bytes
    {
        SizeState::CleanupDue
    } else if snapshot.physical_bytes > policy.cleanup_trigger_bytes
        && snapshot.logical_bytes <= policy.cleanup_target_bytes
    {
        SizeState::ReusableCapacity
    } else {
        SizeState::WithinPolicy
    }
}

fn state_without_measurement(policy: &PolicyStatus, coverage: &Coverage) -> SizeState {
    if matches!(
        coverage,
        Coverage::RowBounded | Coverage::Unmanaged | Coverage::External
    ) {
        SizeState::Unknown
    } else if policy.size_limit_bytes == 0 {
        SizeState::Disabled
    } else {
        SizeState::Unknown
    }
}

#[cfg(test)]
mod tests {
    use agentsight_sqlite_lifecycle::MaintenanceJobState;

    use super::*;
    use crate::database::{DatabaseAccess, DatabaseCoverage, DatabaseRole, DatabaseSpec};

    #[test]
    fn missing_manager_uses_fallback_without_claiming_maintenance() {
        let config = StorageConfig::default();
        let response = collect_storage_status(Path::new("/missing/genai.db"), &config, None);
        let genai = response
            .stores
            .iter()
            .find(|store| store.id == "genai")
            .unwrap();
        assert!(matches!(genai.availability, Availability::Missing));
        assert!(genai.size.is_none());
        assert!(!genai.maintenance.scheduled);
        assert!(!genai.maintenance.worker_running);
        assert!(genai.maintenance.next_run_unix_ms.is_none());
    }

    #[test]
    fn physical_overshoot_above_target_is_cleanup_due() {
        let policy = policy_status(30, 1, 1, "seconds", "trace", 90);
        let snapshot = SizeSnapshot {
            database_bytes: 1_050_000,
            wal_bytes: 0,
            shm_bytes: 0,
            freelist_bytes: 100_000,
            physical_bytes: 1_050_000,
            logical_bytes: 950_000,
        };
        assert!(matches!(
            classify_size(&policy, snapshot),
            SizeState::CleanupDue
        ));
    }

    #[test]
    fn response_v2_has_runtime_fields_without_filesystem_paths() {
        let config = StorageConfig::default();
        let response =
            collect_storage_status(Path::new("/secret/location/genai.db"), &config, None);
        let json = serde_json::to_value(&response).unwrap();
        let encoded = serde_json::to_string(&json).unwrap();
        assert_eq!(json["schema_version"], 2);
        assert_eq!(json["stores"][0]["maintenance"]["scheduled"], false);
        assert!(json["stores"][0]["maintenance"]["last_result"].is_null());
        assert!(json["stores"][0]["maintenance"]["next_run_unix_ms"].is_null());
        assert!(!encoded.contains("/secret/location"));
        assert!(!encoded.contains("base_path"));
    }

    #[test]
    fn manager_path_takes_precedence_without_being_serialized() {
        let path = test_database_path("managed-secret");
        rusqlite::Connection::open(&path).unwrap();
        let manager = DatabaseManager::new(
            DatabaseRole::Server,
            [DatabaseSpec::new(
                DatabaseId::GenAi,
                &path,
                DatabaseAccess::ReadOnly,
                DatabaseCoverage::Full,
            )],
        )
        .unwrap();

        let response = collect_storage_status(
            Path::new("/missing/fallback-genai.db"),
            &StorageConfig::default(),
            Some(&manager),
        );
        let genai = response
            .stores
            .iter()
            .find(|store| store.id == "genai")
            .unwrap();
        assert!(matches!(genai.availability, Availability::Present));
        assert!(
            !serde_json::to_string(&response)
                .unwrap()
                .contains(path.to_string_lossy().as_ref())
        );

        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn maintenance_results_map_to_snake_case_runtime_values() {
        for (result, expected) in [
            (MaintenanceJobResult::Success, "success"),
            (MaintenanceJobResult::Error, "error"),
            (MaintenanceJobResult::LockBusy, "lock_busy"),
            (MaintenanceJobResult::Panicked, "panicked"),
        ] {
            let state = MaintenanceWorkerState {
                running: true,
                heartbeat_unix_ms: 99,
                jobs: vec![MaintenanceJobState {
                    id: DatabaseId::GenAi.as_str().to_owned(),
                    last_attempt_unix_ms: Some(10),
                    last_success_unix_ms: Some(9),
                    last_result: Some(result),
                    consecutive_failures: 2,
                    next_run_unix_ms: 20,
                }],
            };
            let runtime = maintenance_status(DatabaseId::GenAi, Some(&state));
            let json = serde_json::to_value(runtime).unwrap();
            assert_eq!(json["last_result"], expected);
            assert_eq!(json["last_attempt_unix_ms"], 10);
            assert_eq!(json["last_success_unix_ms"], 9);
            assert_eq!(json["consecutive_failures"], 2);
            assert_eq!(json["next_run_unix_ms"], 20);
            assert_eq!(json["worker_running"], true);
            assert_eq!(json["worker_heartbeat_unix_ms"], 99);
        }
    }

    #[test]
    fn unscheduled_primary_does_not_inherit_worker_state() {
        let state = MaintenanceWorkerState {
            running: true,
            heartbeat_unix_ms: 99,
            jobs: vec![MaintenanceJobState {
                id: DatabaseId::GenAi.as_str().to_owned(),
                last_attempt_unix_ms: None,
                last_success_unix_ms: None,
                last_result: None,
                consecutive_failures: 0,
                next_run_unix_ms: 20,
            }],
        };

        let runtime = maintenance_status(DatabaseId::Primary, Some(&state));
        assert!(!runtime.scheduled);
        assert!(!runtime.worker_running);
    }

    #[test]
    fn local_inventory_reports_process_scoped_maintenance_and_partial_stores() {
        let base = test_database_path("local-inventory");
        let config = StorageConfig {
            base_path: base.clone(),
            ..StorageConfig::default()
        };
        let response = collect_local_storage_status(&config.trajectory_path(), &config, None);

        assert_eq!(response.schema_version, 2);
        assert_eq!(
            response
                .stores
                .iter()
                .map(|store| store.id)
                .collect::<Vec<_>>(),
            ["trajectories", "optimization", "reuse", "tokenless"]
        );
        assert!(matches!(response.stores[0].coverage, Coverage::Partial));
        assert!(matches!(response.stores[1].coverage, Coverage::Full));
        assert!(matches!(response.stores[2].coverage, Coverage::Partial));
        assert!(matches!(response.stores[3].coverage, Coverage::External));
        assert!(
            response
                .stores
                .iter()
                .all(|store| !store.maintenance.scheduled)
        );
    }

    #[test]
    fn linux_inventory_includes_reuse_causal_and_partial_enforcement() {
        let config = StorageConfig {
            reuse: PeriodicStoragePolicy::new(11, 12, 13),
            causal: PeriodicStoragePolicy::new(21, 22, 23),
            enforcement: PeriodicStoragePolicy::new(31, 32, 33),
            ..StorageConfig::default()
        };
        let response = collect_storage_status(Path::new("/missing/genai.db"), &config, None);
        let reuse = response
            .stores
            .iter()
            .find(|store| store.id == "reuse")
            .unwrap();
        let causal = response
            .stores
            .iter()
            .find(|store| store.id == "causal")
            .unwrap();
        let enforcement = response
            .stores
            .iter()
            .find(|store| store.id == "enforcement")
            .unwrap();

        assert_eq!(reuse.policy.retention_days, 11);
        assert_eq!(reuse.policy.size_limit_bytes, 12 * 1024 * 1024);
        assert_eq!(reuse.policy.check_interval, 13);
        assert_eq!(causal.policy.retention_days, 21);
        assert_eq!(causal.policy.size_limit_bytes, 22 * 1024 * 1024);
        assert_eq!(causal.policy.check_interval, 23);
        assert_eq!(enforcement.policy.retention_days, 31);
        assert_eq!(enforcement.policy.size_limit_bytes, 32 * 1024 * 1024);
        assert_eq!(enforcement.policy.check_interval, 33);
        assert!(matches!(reuse.coverage, Coverage::Partial));
        assert!(matches!(causal.coverage, Coverage::Partial));
        assert!(matches!(enforcement.coverage, Coverage::Partial));
    }

    fn test_database_path(name: &str) -> PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!(
            "agentsight-storage-status-{name}-{}-{nonce}.db",
            std::process::id()
        ))
    }
}
