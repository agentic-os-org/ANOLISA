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

library_tests! {
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

#[cfg(target_os = "linux")]
#[test]
fn df_operands_on_an_on_demand_mounts_scan_exclude_disabled_checks() {
    let local = ["/", "/data", "/logs"]
        .into_iter()
        .map(str::to_string)
        .collect::<BTreeSet<_>>();
    let config = HealthConfig::default();
    let (paths, incomplete) = df_operand_paths(&config, false, false, true, &local).unwrap();
    assert_eq!(paths.unwrap(), ["/data", "/logs"]);
    assert!(!incomplete);
    let (paths, _) = df_operand_paths(&config, false, true, true, &local).unwrap();
    assert_eq!(paths.unwrap(), ["/", "/data", "/logs"]);
    assert_eq!(
        df_operand_paths(&config, false, false, true, &BTreeSet::new()).err(),
        Some(HealthUnavailableReason::Unsupported)
    );
}

#[cfg(target_os = "linux")]
#[test]
fn df_operands_on_an_on_demand_scan_stay_within_the_target_cap() {
    let local = (0..6)
        .map(|index| format!("/m{index}"))
        .collect::<BTreeSet<_>>();
    let (paths, incomplete) =
        df_operand_paths(&HealthConfig::default(), false, true, true, &local).unwrap();
    assert_eq!(paths.unwrap(), ["/", "/m0", "/m1", "/m2"]);
    assert!(incomplete);
}

#[cfg(target_os = "linux")]
#[test]
fn default_on_demand_scan_keeps_mounts_outside_the_passive_allowlist() {
    let content = "\
36 25 253:0 / / rw,relatime - ext4 /dev/vda1 rw
37 25 259:1 / /boot/efi rw,relatime - vfat /dev/vda2 rw
38 25 0:30 / /run rw,nosuid - tmpfs tmpfs rw
39 25 0:31 / /proc rw - proc proc rw
";
    assert_eq!(
        local_mounts_in(content),
        ["/"].into_iter().map(str::to_string).collect::<BTreeSet<_>>(),
        "the passive allowlist keeps only ext/xfs-style filesystems"
    );
    let on_demand = on_demand_mounts_in(content, &HealthConfig::default().critical_mounts);
    assert_eq!(
        on_demand,
        ["/", "/boot/efi"]
            .into_iter()
            .map(str::to_string)
            .collect::<BTreeSet<_>>(),
        "the default scan keeps the vfat ESP and skips pseudo filesystems"
    );
    let (paths, incomplete) =
        df_operand_paths(&HealthConfig::default(), false, true, true, &on_demand).unwrap();
    assert_eq!(paths.unwrap(), ["/", "/boot/efi"]);
    assert!(!incomplete);
}

#[cfg(target_os = "linux")]
#[test]
fn default_on_demand_scan_keeps_a_critical_mount_on_an_ignored_filesystem() {
    let content = "\
36 25 253:0 / / rw,relatime - ext4 /dev/vda1 rw
37 25 259:1 / /boot/efi rw,relatime - vfat /dev/vda2 rw
38 25 0:30 / /run rw,nosuid - tmpfs tmpfs rw
39 25 0:31 / /proc rw - proc proc rw
40 25 0:55 / /dev/shm rw,nosuid - tmpfs shm rw
";
    let config = HealthConfig {
        critical_mounts: vec!["/".to_string(), "/dev/shm".to_string()],
        ..HealthConfig::default()
    };
    let on_demand = on_demand_mounts_in(content, &config.critical_mounts);
    assert_eq!(
        on_demand,
        ["/", "/boot/efi", "/dev/shm"]
            .into_iter()
            .map(str::to_string)
            .collect::<BTreeSet<_>>(),
        "a declared critical mount stays an operand even on an ignored filesystem"
    );
    let (paths, incomplete) = df_operand_paths(&config, false, true, true, &on_demand).unwrap();
    assert_eq!(paths.unwrap(), ["/", "/boot/efi", "/dev/shm"]);
    assert!(!incomplete);
    let output = "\
Filesystem     Type 1024-blocks Used Available Capacity Mounted on
/dev/shm      tmpfs      65536 1024     64512       2% /dev/shm
";
    let facts = parse_df_p(output, &config.critical_mounts).expect("critical row survives parsing");
    assert!(
        facts.mounts.iter().any(|row| row.mount == "/dev/shm"),
        "the sampled critical row is reported, so the operand was not wasted"
    );
}

#[test]
fn disk_without_enabled_checks_is_unsupported() {
    let _guard = crate::diagnostics::test_env::env_guard();
    assert_eq!(
        read_disk_sample(&HealthConfig::default(), false, false, false).err(),
        Some(HealthUnavailableReason::Unsupported)
    );
}
}
