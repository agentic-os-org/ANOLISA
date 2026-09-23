//! Read-only SQLite policy and capacity status.

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use agentsight_sqlite_lifecycle::{
    ConnectionMode, ConnectionOptions, SizeSnapshot, measure_database, open_connection,
};
use serde::Serialize;

use crate::config::{InsertStoragePolicy, PeriodicStoragePolicy, StorageConfig};

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
pub fn collect_storage_status(genai_path: &Path, config: &StorageConfig) -> StorageStatusResponse {
    let private = config.base_path.join(".agentsight-private");
    let mut stores = vec![
        measured_insert_store(
            "primary",
            config.primary_path(),
            config.primary,
            "trace",
            100,
        ),
        measured_insert_store(
            "genai",
            genai_path.to_path_buf(),
            config.genai,
            "trace_and_serve",
            90,
        ),
        measured_periodic_store(
            "interruptions",
            config.interruption_path(),
            config.interruptions,
            "trace",
            90,
        ),
        measured_periodic_store(
            "trajectories",
            config.trajectory_path(),
            config.trajectories,
            "trajectory_collector",
            90,
        ),
        measured_periodic_store(
            "optimization",
            config.optimization_path(),
            config.optimization,
            "serve",
            90,
        ),
        measured_periodic_store(
            "security_audit",
            private.join(crate::config::SECURITY_AUDIT_DB_NAME),
            config.security_audit,
            "serve",
            90,
        ),
        measured_store(
            "enforcement",
            private.join(crate::config::ENFORCEMENT_DB_NAME),
            PolicyStatus {
                retention_days: 0,
                size_limit_bytes: 0,
                cleanup_trigger_bytes: 0,
                cleanup_target_bytes: 0,
                check_interval: 0,
                check_interval_unit: "none",
                enforced_by: "serve",
            },
            Coverage::RowBounded,
        ),
    ];
    stores.push(StoreStatus {
        id: "tokenless",
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
    });

    response(stores)
}

/// Collects the subset available in local trajectory-viewer mode.
pub fn collect_local_storage_status(
    trajectory_path: &Path,
    optimization_path: &Path,
) -> StorageStatusResponse {
    response(vec![
        measured_periodic_store(
            "trajectories",
            trajectory_path.to_path_buf(),
            PeriodicStoragePolicy {
                retention_days: 30,
                max_db_size_mb: 500,
                check_interval_secs: 300,
            },
            "trajectory_collector",
            90,
        ),
        measured_store(
            "optimization",
            optimization_path.to_path_buf(),
            PolicyStatus {
                retention_days: 0,
                size_limit_bytes: 0,
                cleanup_trigger_bytes: 0,
                cleanup_target_bytes: 0,
                check_interval: 0,
                check_interval_unit: "none",
                enforced_by: "local_viewer",
            },
            Coverage::Unmanaged,
        ),
    ])
}

fn response(stores: Vec<StoreStatus>) -> StorageStatusResponse {
    StorageStatusResponse {
        schema_version: 1,
        observed_at_unix_ms: SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|duration| u64::try_from(duration.as_millis()).unwrap_or(u64::MAX))
            .unwrap_or(0),
        stores,
    }
}

fn measured_insert_store(
    id: &'static str,
    path: PathBuf,
    policy: InsertStoragePolicy,
    enforced_by: &'static str,
    target_percent: u64,
) -> StoreStatus {
    measured_store(
        id,
        path,
        policy_status(
            policy.retention_days,
            policy.max_db_size_mb,
            policy.check_interval_inserts,
            "inserts",
            enforced_by,
            target_percent,
        ),
        Coverage::Full,
    )
}

fn measured_periodic_store(
    id: &'static str,
    path: PathBuf,
    policy: PeriodicStoragePolicy,
    enforced_by: &'static str,
    target_percent: u64,
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
        Coverage::Full,
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
    id: &'static str,
    path: PathBuf,
    policy: PolicyStatus,
    coverage: Coverage,
) -> StoreStatus {
    if !path.is_file() {
        return StoreStatus {
            id,
            availability: Availability::Missing,
            size: None,
            size_state: state_without_measurement(&policy, &coverage),
            policy,
            coverage,
        };
    }

    let options = ConnectionOptions {
        mode: ConnectionMode::ReadOnlyExisting,
        enable_wal: false,
        ..ConnectionOptions::default()
    };
    let snapshot =
        open_connection(&path, options).and_then(|connection| measure_database(&path, &connection));
    match snapshot {
        Ok(snapshot) => {
            let size_state = if matches!(coverage, Coverage::Full) {
                classify_size(&policy, snapshot)
            } else {
                SizeState::Unknown
            };
            StoreStatus {
                id,
                availability: Availability::Present,
                size: Some(snapshot.into()),
                policy,
                coverage,
                size_state,
            }
        }
        Err(_) => StoreStatus {
            id,
            availability: Availability::Error,
            size: None,
            size_state: SizeState::Unknown,
            policy,
            coverage,
        },
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
    use super::*;

    #[test]
    fn missing_store_does_not_report_zero_usage() {
        let config = StorageConfig::default();
        let response = collect_storage_status(Path::new("/missing/genai.db"), &config);
        let genai = response
            .stores
            .iter()
            .find(|store| store.id == "genai")
            .unwrap();
        assert!(matches!(genai.availability, Availability::Missing));
        assert!(genai.size.is_none());
    }

    #[test]
    fn physical_overshoot_above_target_is_cleanup_due() {
        let policy = policy_status(30, 1, 1, "inserts", "trace", 90);
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
    fn response_does_not_serialize_filesystem_paths() {
        let config = StorageConfig::default();
        let response = collect_storage_status(Path::new("/secret/location/genai.db"), &config);
        let json = serde_json::to_string(&response).unwrap();
        assert!(!json.contains("/secret/location"));
        assert!(!json.contains("base_path"));
    }
}
