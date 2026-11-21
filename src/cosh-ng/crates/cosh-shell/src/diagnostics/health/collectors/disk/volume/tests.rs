use super::*;
use crate::diagnostics::health::model::{HealthCollector, HealthFactCoverage, HealthFactValue};

fn raw(frsize: u64, blocks: u64, bfree: u64, bavail: u64, files: u64, favail: u64) -> RawStatfs {
    RawStatfs {
        frsize,
        blocks,
        bfree,
        bavail,
        files,
        favail,
    }
}

#[test]
fn mountinfo_parses_identity_fields_and_decodes_escapes() {
    let content = "36 35 98:0 / / rw,noatime shared:1 - ext4 /dev/sda1 rw\n\
                   41 36 0:42 / /data rw shared:2 - xfs /dev/sdb1 rw\n\
                   52 36 0:43 /sub /mnt/bind rw - ext4 /dev/sda1 rw\n\
                   60 36 0:44 / /mnt/with\\040space rw - autofs systemd-1 rw\n";
    let entries = parse_mountinfo(content);
    assert_eq!(entries.len(), 4);
    assert_eq!(entries[0].mnt_id, 36);
    assert_eq!(entries[0].dev, "98:0");
    assert_eq!(entries[0].root, "/");
    assert_eq!(entries[0].fs_type, "ext4");
    assert_eq!(entries[2].root, "/sub");
    assert_eq!(entries[3].mount_point, "/mnt/with space");
    assert_eq!(entries[3].fs_type, "autofs");
}

#[test]
fn mountinfo_malformed_lines_are_dropped_not_zero_filled() {
    let entries = parse_mountinfo("garbage\n36 35 98:0 / / rw - ext4\n");
    assert_eq!(entries.len(), 1);
}

#[test]
fn mountinfo_detects_exact_enforced_project_quota_options() {
    let content = "36 35 98:0 / / rw,prjquota - ext4 /dev/sda1 rw\n\
                   41 36 0:42 / /a rw - xfs /dev/sdb1 rw,pquota\n\
                   52 36 0:43 / /b rw - xfs /dev/sdc1 rw,pqnoenforce\n\
                   53 36 0:44 / /c rw - xfs /dev/sdd1 rw,usrquota,nopquota\n";
    let entries = parse_mountinfo(content);
    assert_eq!(
        entries
            .iter()
            .map(|entry| entry.project_quota)
            .collect::<Vec<_>>(),
        [true, true, false, false],
    );
}

#[test]
fn fdinfo_mnt_id_extraction() {
    let content = "pos:\t0\nflags:\t02000002\nmnt_id:\t29\nino:\t123\n";
    assert_eq!(parse_fdinfo_mnt_id(content), Some(29));
    assert_eq!(parse_fdinfo_mnt_id("pos: 0\n"), None);
}

#[test]
fn bytes_valid_zero_available_is_a_fact_not_unknown() {
    let stats = validate_bytes(&raw(4096, 1000, 50, 0, 100, 10)).unwrap();
    assert_eq!(stats.bavail_bytes, 0);
    assert_eq!(stats.bfree_bytes, 50 * 4096);
    assert_eq!(stats.used_bytes, 950 * 4096);
}

#[test]
fn bytes_boundary_values_stay_exact() {
    let gib = 1024u64 * 1024 * 1024;
    let stats = validate_bytes(&raw(1, 3 * gib, 2 * gib, 2 * gib, 0, 0)).unwrap();
    assert_eq!(stats.bavail_bytes, 2 * gib);
    assert_eq!(stats.used_bytes, gib);
}

#[test]
fn bytes_malformed_shapes_are_unknown() {
    assert!(validate_bytes(&raw(0, 100, 10, 10, 0, 0)).is_none());
    assert!(validate_bytes(&raw(4096, 0, 0, 0, 0, 0)).is_none());
    assert!(validate_bytes(&raw(4096, 100, 101, 10, 0, 0)).is_none());
    assert!(validate_bytes(&raw(4096, 100, 10, 11, 0, 0)).is_none());
    assert!(validate_bytes(&raw(u64::MAX, u64::MAX, u64::MAX, u64::MAX, 0, 0)).is_none());
}

#[test]
fn inodes_valid_zero_and_sentinel_shapes() {
    let stats = validate_inodes(&raw(4096, 1000, 100, 10, 10000, 0)).unwrap();
    assert_eq!(stats.favail, 0);
    assert!(validate_inodes(&raw(4096, 1000, 100, 10, 0, 0)).is_none());
    assert!(validate_inodes(&raw(4096, 1000, 100, 10, 100, 101)).is_none());
    let max = validate_inodes(&raw(4096, 1000, 100, 10, u64::MAX, u64::MAX)).unwrap();
    assert_eq!(max.files, u64::MAX);
}

#[test]
fn inode_applicability_is_ext4_only() {
    assert!(inode_volume_fs("ext4"));
    for fs in ["xfs", "overlay", "btrfs", "tmpfs", "ext3"] {
        assert!(!inode_volume_fs(fs), "{fs}");
    }
    assert!(byte_volume_fs("ext4"));
    assert!(byte_volume_fs("xfs"));
    assert!(!byte_volume_fs("overlay"));
    assert!(!byte_volume_fs("autofs"));
}

#[test]
fn declared_targets_cap_at_four_and_count_over_limit() {
    let config = HealthConfig {
        scan_mounts: Some(vec![
            "/".into(),
            "/a".into(),
            "/b".into(),
            "/c".into(),
            "/d".into(),
            "/a".into(),
        ]),
        ..HealthConfig::default()
    };
    let (targets, over_limit) = declared_targets(&config, true, true);
    assert_eq!(targets, vec!["/", "/a", "/b", "/c"]);
    assert_eq!(over_limit, 1);
    let (targets, over_limit) = declared_targets(&HealthConfig::default(), true, true);
    assert_eq!(targets, vec!["/"]);
    assert_eq!(over_limit, 0);
}

library_tests! {
#[test]
fn declared_targets_follow_enabled_checks() {
    let config = HealthConfig {
        scan_mounts: Some(vec!["/data".into(), "/logs".into()]),
        ..HealthConfig::default()
    };
    assert_eq!(
        declared_targets(&config, true, true).0,
        vec!["/", "/data", "/logs"]
    );
    assert_eq!(declared_targets(&config, true, false).0, vec!["/"]);
    assert_eq!(
        declared_targets(&config, false, true).0,
        vec!["/data", "/logs"]
    );
    let (targets, over_limit) = declared_targets(&config, false, false);
    assert!(targets.is_empty());
    assert_eq!(over_limit, 0);
}

#[test]
fn declared_target_cap_counts_only_enabled_targets() {
    let config = HealthConfig {
        scan_mounts: Some((0..5).map(|index| format!("/m{index}")).collect()),
        ..HealthConfig::default()
    };
    let (targets, over_limit) = declared_targets(&config, true, false);
    assert_eq!(targets, ["/"]);
    assert_eq!(over_limit, 0);
    let (targets, over_limit) = declared_targets(&config, true, true);
    assert_eq!(targets, ["/", "/m0", "/m1", "/m2"]);
    assert_eq!(over_limit, 2);
    let (targets, over_limit) = declared_targets(&config, false, true);
    assert_eq!(targets, ["/m0", "/m1", "/m2", "/m3"]);
    assert_eq!(over_limit, 1);
}

#[test]
fn read_volume_sample_root_only_skips_declared_mounts() {
    let config = HealthConfig {
        scan_mounts: Some(vec!["/cosh/no-such-mount".into()]),
        ..HealthConfig::default()
    };
    let sample = read_volume_sample(&config, true, false);
    assert!(sample.volumes.iter().all(|volume| volume.mount == "/"));
}

#[test]
fn read_volume_sample_mounts_only_skips_root() {
    let config = HealthConfig {
        scan_mounts: Some(vec!["/cosh/no-such-mount".into()]),
        ..HealthConfig::default()
    };
    let sample = read_volume_sample(&config, false, true);
    assert!(!sample.volumes.iter().any(|volume| volume.mount == "/"));
    if sample.supported {
        assert_eq!(sample.volumes.len(), 1);
        assert_eq!(
            sample.volumes[0].unknown,
            Some(VolumeUnknown::NotMountPoint)
        );
    }
}

#[test]
fn volume_proven_for_check_uses_only_its_own_targets() {
    let mut sample = VolumeSample {
        volumes: vec![proven_root()],
        over_limit: 0,
        mnt_ns: None,
        supported: true,
    };
    assert!(volume_proven_for_check(&sample, "filesystem.root"));
    assert!(!volume_proven_for_check(&sample, "filesystem.mounts"));
    sample
        .volumes
        .push(VolumeFacts::unknown("/data", VolumeUnknown::Autofs));
    assert!(!volume_proven_for_check(&sample, "filesystem.mounts"));
    sample.volumes.push(VolumeFacts {
        mount: "/data".into(),
        ..proven_root()
    });
    assert!(volume_proven_for_check(&sample, "filesystem.mounts"));
    assert!(volume_proven_for_check(&sample, "filesystem.root"));
}
}

fn sample_with(volume: VolumeFacts) -> VolumeSample {
    VolumeSample {
        volumes: vec![volume],
        over_limit: 0,
        mnt_ns: Some("mnt:[4026531840]".into()),
        supported: true,
    }
}

fn proven_root() -> VolumeFacts {
    VolumeFacts {
        mount: "/".into(),
        fs_type: Some("ext4".into()),
        device: Some("253:0".into()),
        fsid: Some("1:2".into()),
        bytes: Some(ByteStats {
            bavail_bytes: 0,
            bfree_bytes: 4096,
            used_bytes: 4096 * 999,
        }),
        inodes: Some(InodeStats {
            files: 10000,
            favail: 500,
        }),
        inode_applicable: true,
        readonly: Some(false),
        expected_writable: false,
        project_quota: false,
        unknown: None,
    }
}

fn record(sample: &VolumeSample, check_id: &str) -> crate::diagnostics::health::HealthScanReport {
    let mut builder = HealthReportBuilder::for_started_at(1);
    builder.start_check_id(
        check_id,
        HealthCollector::Disk,
        HealthFactScope::Unknown,
        HealthFactCoverage::Snapshot,
        1,
    );
    record_volumes(&mut builder, sample, Some(check_id), 0);
    builder.add_check_done(check_id).complete_check();
    builder.finish(2)
}

#[test]
fn proven_volume_records_raw_fields_under_root_check() {
    let report = record(&sample_with(proven_root()), "filesystem.root");
    let fact = |key: &str| {
        report
            .facts
            .iter()
            .find(|fact| fact.key == key)
            .map(|fact| fact.value.clone())
    };
    assert_eq!(
        fact("filesystem.mount./.volume_proven"),
        Some(HealthFactValue::Bool(true))
    );
    assert_eq!(
        fact("filesystem.mount./.volume_bavail_bytes"),
        Some(HealthFactValue::Unsigned(0))
    );
    assert_eq!(
        fact("filesystem.mount./.volume_files"),
        Some(HealthFactValue::Unsigned(10000))
    );
    assert_eq!(
        fact("filesystem.mount./.volume_inode_applicable"),
        Some(HealthFactValue::Bool(true))
    );
    assert_eq!(
        fact("filesystem.mount./.volume_readonly"),
        Some(HealthFactValue::Bool(false))
    );
    assert_eq!(
        fact("filesystem.volume_mnt_ns"),
        Some(HealthFactValue::String("mnt:[4026531840]".into()))
    );
}

library_tests! {
#[test]
fn proven_readonly_volume_records_readonly_true() {
    let mut volume = proven_root();
    volume.readonly = Some(true);
    let report = record(&sample_with(volume), "filesystem.root");
    let fact = |key: &str| {
        report
            .facts
            .iter()
            .find(|fact| fact.key == key)
            .map(|fact| fact.value.clone())
    };
    assert_eq!(
        fact("filesystem.mount./.volume_proven"),
        Some(HealthFactValue::Bool(true))
    );
    assert_eq!(
        fact("filesystem.mount./.volume_readonly"),
        Some(HealthFactValue::Bool(true))
    );
}
}

#[test]
fn project_quota_fact_preserves_a_bound_readonly_mount() {
    let mut volume = proven_root();
    volume.project_quota = true;
    volume.readonly = Some(true);
    volume.expected_writable = true;
    let report = record(&sample_with(volume), "filesystem.root");
    assert!(report.facts.iter().any(|fact| {
        fact.key == "filesystem.mount./.volume_project_quota"
            && fact.value == HealthFactValue::Bool(true)
    }));
    let findings = crate::diagnostics::health::rules::ro::ro_findings(&report.facts);
    assert!(findings.iter().any(|finding| finding.id == "J18"));
}

#[test]
fn unknown_volume_records_reason_and_no_numeric_facts() {
    let volume = VolumeFacts::unknown("/data", VolumeUnknown::Autofs);
    let report = record(&sample_with(volume), "filesystem.mounts");
    let fact = |key: &str| {
        report
            .facts
            .iter()
            .find(|fact| fact.key == key)
            .map(|fact| fact.value.clone())
    };
    assert_eq!(
        fact("filesystem.mount./data.volume_proven"),
        Some(HealthFactValue::Bool(false))
    );
    assert_eq!(
        fact("filesystem.mount./data.volume_unknown_reason"),
        Some(HealthFactValue::String("autofs".into()))
    );
    assert!(fact("filesystem.mount./data.volume_bavail_bytes").is_none());
    assert!(fact("filesystem.mount./data.volume_files").is_none());
}

#[test]
fn root_check_does_not_record_other_mounts() {
    let mut sample = sample_with(proven_root());
    sample.volumes.push(VolumeFacts {
        mount: "/data".into(),
        ..proven_root()
    });
    let report = record(&sample, "filesystem.root");
    assert!(report
        .facts
        .iter()
        .all(|fact| !fact.key.starts_with("filesystem.mount./data.")));
}

library_tests! {
#[test]
fn mounts_check_records_only_non_root_volumes() {
    let mut sample = sample_with(proven_root());
    sample.volumes.push(VolumeFacts {
        mount: "/data".into(),
        ..proven_root()
    });
    let report = record(&sample, "filesystem.mounts");
    assert!(report
        .facts
        .iter()
        .all(|fact| !fact.key.starts_with("filesystem.mount./.volume")));
    assert!(report
        .facts
        .iter()
        .any(|fact| fact.key == "filesystem.mount./data.volume_proven"));
}
}

#[test]
fn unsupported_platform_records_nothing() {
    let sample = VolumeSample {
        volumes: Vec::new(),
        over_limit: 0,
        mnt_ns: None,
        supported: false,
    };
    let report = record(&sample, "filesystem.root");
    assert!(report.facts.is_empty());
}
