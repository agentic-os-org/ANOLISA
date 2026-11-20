use super::*;
use crate::diagnostics::health::model::{HealthCheckStatus, HealthFactCoverage};

#[test]
fn disk_nonzero_exit_keeps_root_and_limits_gap_to_mounts() {
    let _guard = crate::diagnostics::test_env::env_guard();
    let directory = tempfile::tempdir().unwrap();
    let missing = directory
        .path()
        .join("missing")
        .to_string_lossy()
        .into_owned();
    let config = HealthConfig {
        scan_mounts: Some(vec![missing.clone()]),
        ..HealthConfig::default()
    };
    let sample = read_disk_sample(&config, false, true, true)
        .expect("valid root df row survives another path failure");
    assert!(sample.root_used_ratio.is_some());
    let mut builder = HealthReportBuilder::for_started_at(1);
    for id in ["filesystem.root", "filesystem.mounts"] {
        builder.start_check_id(
            id,
            HealthCollector::Disk,
            HealthFactScope::Unknown,
            HealthFactCoverage::Snapshot,
            1,
        );
        sample.record(&mut builder, 0);
        builder.add_check_done(id).complete_check();
    }
    let report = builder.finish(2);
    let root = report
        .check_results
        .iter()
        .find(|check| check.id == "filesystem.root")
        .unwrap();
    assert_eq!(root.status, HealthCheckStatus::Ok);
    assert!(root.gaps.is_empty());
    assert!(report
        .facts
        .iter()
        .any(|fact| fact.key == "filesystem.root_used_ratio"));
    let mounts = report
        .check_results
        .iter()
        .find(|check| check.id == "filesystem.mounts")
        .unwrap();
    assert_eq!(mounts.status, HealthCheckStatus::Unavailable);
    assert_eq!(mounts.gaps.len(), 1);
    assert!(!report.facts.iter().any(|fact| fact.key.contains(&missing)));
}

#[test]
fn disk_nonzero_exit_without_valid_rows_remains_unavailable() {
    let _guard = crate::diagnostics::test_env::env_guard();
    let directory = tempfile::tempdir().unwrap();
    let config = HealthConfig {
        scan_mounts: Some(vec![directory
            .path()
            .join("missing")
            .to_string_lossy()
            .into_owned()]),
        ..HealthConfig::default()
    };
    assert!(read_disk_sample(&config, false, false, true).is_err());
}

#[test]
fn disk_root_only_never_samples_declared_mounts() {
    let _guard = crate::diagnostics::test_env::env_guard();
    let config = HealthConfig {
        scan_mounts: Some((0..5).map(|index| format!("/cosh/m{index}")).collect()),
        ..HealthConfig::default()
    };
    let sample = read_disk_sample(&config, false, true, false)
        .expect("declared mounts stay unsampled when the mounts check is disabled");
    assert!(sample.mounts.iter().all(|row| row.mount == "/"));
    assert!(sample.root_used_ratio.is_some());
}

#[test]
fn disk_mounts_only_drops_the_root_row() {
    let _guard = crate::diagnostics::test_env::env_guard();
    let directory = tempfile::tempdir().unwrap();
    let config = HealthConfig {
        scan_mounts: Some(vec![directory.path().to_string_lossy().into_owned()]),
        ..HealthConfig::default()
    };
    let sample = read_disk_sample(&config, false, false, true)
        .expect("df on a non-mount path still yields the containing row");
    assert!(sample.mounts.iter().all(|row| row.mount != "/"));
}

#[test]
fn disk_without_enabled_checks_is_unsupported() {
    let _guard = crate::diagnostics::test_env::env_guard();
    assert_eq!(
        read_disk_sample(&HealthConfig::default(), false, false, false).err(),
        Some(HealthUnavailableReason::Unsupported)
    );
}
