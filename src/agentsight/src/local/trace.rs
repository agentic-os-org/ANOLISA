//! macOS trace implementation — trajectory collector only (no eBPF).

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use agentsight_sqlite_lifecycle::{LifecycleError, MaintenanceJob};
use agentsight_trajectory_collector::{
    CollectorConfig, TrajectoryMaintenancePolicy, TrajectoryStore, run_collector_loop,
};

use crate::config::{AgentsightConfig, StorageConfig};
use crate::database::{
    DatabaseAccess, DatabaseCoverage, DatabaseId, DatabaseManager, DatabaseManagerError,
    DatabaseRole, DatabaseSpec,
};

/// Run the trajectory collector loop on macOS.
///
/// Opens (or creates) the trajectory SQLite DB through the local process registry,
/// starts its maintenance job, then collects until Ctrl+C.
///
/// Logging is initialized here because the collector reports per-file scan
/// failures through `log::warn!`; without a logger installed those failures
/// are dropped and a stalled collector looks identical to an idle one.
pub fn run_local_trace(verbose: bool, config: AgentsightConfig) {
    crate::config::init_logging(verbose, None);

    let db_path = config.storage.trajectory_path();
    if let Some(parent) = db_path.parent()
        && let Err(error) = std::fs::create_dir_all(parent)
    {
        eprintln!("Failed to create trajectory database directory {parent:?}: {error}");
        std::process::exit(1);
    }

    let manager = match DatabaseManager::new(
        DatabaseRole::LocalTrace,
        [DatabaseSpec::new(
            DatabaseId::Trajectories,
            &db_path,
            DatabaseAccess::ReadWrite,
            DatabaseCoverage::Partial,
        )],
    ) {
        Ok(manager) => manager,
        Err(error) => {
            eprintln!("Failed to register trajectory database: {error}");
            std::process::exit(1);
        }
    };

    let store =
        match manager.open_read_write(DatabaseId::Trajectories, TrajectoryStore::new_with_path) {
            Ok(store) => Arc::new(store),
            Err(error) => {
                eprintln!("Failed to open trajectory store at {db_path:?}: {error}");
                std::process::exit(1);
            }
        };

    let collector_config = CollectorConfig {
        scan_interval_secs: config.features.trajectory_scan_interval_secs,
        scan_dirs: config
            .features
            .trajectory_scan_dirs
            .as_ref()
            .map(|dirs| dirs.iter().map(std::path::PathBuf::from).collect())
            .or_else(crate::local::server::local_trajectory_scan_dirs),
        maintenance: TrajectoryMaintenancePolicy {
            retention_days: config.storage.trajectories.retention_days,
            max_db_size_mb: config.storage.trajectories.max_db_size_mb,
        },
    };

    let jobs = match trajectory_maintenance_jobs(&manager, &config.storage, Arc::clone(&store)) {
        Ok(jobs) => jobs,
        Err(error) => {
            eprintln!("Failed to configure trajectory maintenance: {error}");
            std::process::exit(1);
        }
    };
    if let Err(error) = manager.start_maintenance(jobs) {
        eprintln!("Failed to start trajectory maintenance: {error}");
        std::process::exit(1);
    }

    let stop = Arc::new(AtomicBool::new(true));
    let stop_clone = Arc::clone(&stop);

    if let Err(error) = ctrlc::set_handler(move || {
        println!("\nShutting down trajectory collector...");
        stop_clone.store(false, Ordering::SeqCst);
    }) {
        log::warn!("Could not install the Ctrl+C handler: {error}");
    }

    println!("Trajectory collector running. Press Ctrl+C to stop.");
    run_collector_loop(store, &collector_config, &stop);

    if let Err(error) = manager.stop_maintenance() {
        log::warn!("Trajectory maintenance worker shutdown failed: {error}");
    }
}

fn trajectory_maintenance_jobs(
    manager: &DatabaseManager,
    storage_config: &StorageConfig,
    store: Arc<TrajectoryStore>,
) -> Result<Vec<Box<dyn MaintenanceJob>>, DatabaseManagerError> {
    let policy = storage_config.trajectories;
    if policy.check_interval_secs == 0 {
        return Ok(Vec::new());
    }

    let maintenance_policy = TrajectoryMaintenancePolicy {
        retention_days: policy.retention_days,
        max_db_size_mb: policy.max_db_size_mb,
    };
    let job = manager.maintenance_job(
        DatabaseId::Trajectories,
        Duration::from_secs(policy.check_interval_secs),
        move || {
            store
                .maintain(maintenance_policy)
                .map(|_| ())
                .map_err(|error| LifecycleError::MaintenanceJobFailed(error.to_string()))
        },
    )?;
    Ok(vec![job])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_path(name: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!(
            "agentsight-local-trace-{name}-{}.db",
            std::process::id()
        ))
    }

    #[test]
    fn trajectory_schedule_registers_only_trajectory() {
        let path = test_path("scheduled");
        let manager = DatabaseManager::new(
            DatabaseRole::LocalTrace,
            [DatabaseSpec::new(
                DatabaseId::Trajectories,
                &path,
                DatabaseAccess::ReadWrite,
                DatabaseCoverage::Full,
            )],
        )
        .unwrap();
        let store = Arc::new(TrajectoryStore::new_with_path(&path).unwrap());
        let config = StorageConfig::default();

        let jobs = trajectory_maintenance_jobs(&manager, &config, Arc::clone(&store)).unwrap();

        assert_eq!(jobs.len(), 1);
        assert_eq!(jobs[0].id(), DatabaseId::Trajectories.as_str());
        assert_eq!(
            jobs[0].interval(),
            Duration::from_secs(config.trajectories.check_interval_secs)
        );
        drop(jobs);
        drop(store);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn trajectory_schedule_skips_zero_interval() {
        let path = test_path("zero-interval");
        let manager = DatabaseManager::new(
            DatabaseRole::LocalTrace,
            [DatabaseSpec::new(
                DatabaseId::Trajectories,
                &path,
                DatabaseAccess::ReadWrite,
                DatabaseCoverage::Full,
            )],
        )
        .unwrap();
        let store = Arc::new(TrajectoryStore::new_with_path(&path).unwrap());
        let mut config = StorageConfig::default();
        config.trajectories.check_interval_secs = 0;

        let jobs = trajectory_maintenance_jobs(&manager, &config, Arc::clone(&store)).unwrap();

        assert!(jobs.is_empty());
        drop(store);
        let _ = std::fs::remove_file(path);
    }
}
