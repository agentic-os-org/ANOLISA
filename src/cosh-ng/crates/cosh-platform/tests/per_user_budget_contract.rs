use chrono::{Duration, Utc};
use cosh_platform::audit::retention::execute_retention;
use cosh_platform::audit::store::{AuditDurability, AuditSegmentWriter};
use cosh_types::audit::*;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use uuid::Uuid;

fn event() -> AuditEventV1 {
    let now = Utc::now();
    AuditEventV1::new(
        Uuid::new_v4().to_string(),
        KnownAuditEventType::AuditDegraded.into(),
        now,
        now,
        0,
        AuditComponent {
            name: AuditComponentName::CoshCore,
            version: "0.27.0".into(),
        },
        AuditIdentity::default(),
        AuditActor {
            kind: AuditActorKind::System,
            uid: None,
            euid: None,
        },
        AuditEventOutcome {
            status: AuditOutcomeStatus::Degraded,
            code: None,
            retryable: false,
        },
        AuditSubject {
            kind: "audit".into(),
            name: None,
        },
        &AuditControlData {
            operation: Some("budget_probe".into()),
            ..Default::default()
        },
        AuditRedaction {
            policy_version: "audit-redaction-v1".into(),
            status: AuditRedactionStatus::Clean,
            fields: vec![],
        },
    )
    .unwrap()
}

fn segment_bytes(root: &Path) -> u64 {
    fn total(path: &Path) -> u64 {
        fs::read_dir(path)
            .unwrap()
            .map(|entry| {
                let entry = entry.unwrap();
                if entry.file_type().unwrap().is_dir() {
                    total(&entry.path())
                } else if entry.file_type().unwrap().is_file() {
                    entry.metadata().unwrap().len()
                } else {
                    0
                }
            })
            .sum()
    }
    total(&root.join("v1/segments"))
}

#[test]
fn audit_default_30_day_closed_retention_preserves_unmanaged_sentinel() {
    let temp = tempfile::tempdir().unwrap();
    fs::set_permissions(temp.path(), fs::Permissions::from_mode(0o700)).unwrap();
    let root = temp.path().canonicalize().unwrap();
    let settings = AuditSettings::default();
    assert_eq!(settings.retention_days, 30);
    assert_eq!(settings.max_disk_bytes, 100 * 1024 * 1024);
    let mut writer = AuditSegmentWriter::create(&root, AuditComponentName::CoshCore).unwrap();
    writer
        .append(&mut event(), AuditDurability::Ordinary)
        .unwrap();
    writer.close().unwrap();
    let sentinel = root.join("unmanaged-sentinel");
    fs::write(&sentinel, b"foreign-owned-content").unwrap();
    execute_retention(&root, &settings, Utc::now() + Duration::days(29)).unwrap();
    assert!(segment_bytes(&root) > 0);
    execute_retention(&root, &settings, Utc::now() + Duration::days(31)).unwrap();
    assert_eq!(segment_bytes(&root), 0);
    assert_eq!(fs::read(sentinel).unwrap(), b"foreign-owned-content");
}

#[test]
fn audit_live_segments_are_not_deleted_by_retention() {
    let temp = tempfile::tempdir().unwrap();
    fs::set_permissions(temp.path(), fs::Permissions::from_mode(0o700)).unwrap();
    let root = temp.path().canonicalize().unwrap();
    let mut writer = AuditSegmentWriter::create(&root, AuditComponentName::CoshCore).unwrap();
    writer
        .append(&mut event(), AuditDurability::SecurityBoundary)
        .unwrap();
    let before = segment_bytes(&root);
    execute_retention(
        &root,
        &AuditSettings::default(),
        Utc::now() + Duration::days(31),
    )
    .unwrap();
    assert_eq!(segment_bytes(&root), before);
    writer
        .append(&mut event(), AuditDurability::Ordinary)
        .unwrap();
    assert!(segment_bytes(&root) > before);
}

#[test]
fn audit_continuous_multiwriter_respects_actual_default_100_mib() {
    let temp = tempfile::tempdir().unwrap();
    fs::set_permissions(temp.path(), fs::Permissions::from_mode(0o700)).unwrap();
    let root = temp.path().canonicalize().unwrap();
    let settings = AuditSettings::default();
    assert_eq!(settings.max_disk_bytes, 100 * 1024 * 1024);
    let mut writers: Vec<_> = (0..8)
        .map(|_| AuditSegmentWriter::create(&root, AuditComponentName::CoshCore).unwrap())
        .collect();
    let mut accepted = 0;
    let record_bytes = serde_json::to_vec(&event()).unwrap().len() as u64;
    let finite_writes = (settings.max_disk_bytes / record_bytes + 1024) as usize;
    for index in 0..finite_writes {
        let result = writers[index % 8].append(&mut event(), AuditDurability::Ordinary);
        if result.is_err() {
            assert!(segment_bytes(&root) <= settings.max_disk_bytes);
            return;
        }
        accepted += 1;
        if accepted % 64 == 0 {
            let actual = segment_bytes(&root);
            assert!(actual <= settings.max_disk_bytes,
                "real AuditSegmentWriter accepted {accepted} records into {actual} bytes across eight live writers; default per-user cap is {}", settings.max_disk_bytes);
        }
    }
    assert!(segment_bytes(&root) <= settings.max_disk_bytes);
}
