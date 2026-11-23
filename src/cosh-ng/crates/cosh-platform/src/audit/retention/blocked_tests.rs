use super::*;
use std::os::unix::fs::PermissionsExt;

fn private_root() -> crate::audit::AuditTestDir {
    let root = crate::audit::AuditTestDir::create();
    ensure_private_dir(&root.path().join("v1")).unwrap();
    ensure_private_dir(&root.path().join("v1/segments")).unwrap();
    root
}

fn expired_segment(root: &Path, name: &str, bytes: &[u8]) -> PathBuf {
    let directory = root.join("v1/segments/2020-01-01");
    ensure_private_dir(&directory).unwrap();
    let path = directory.join(name);
    std::fs::write(&path, bytes).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
    File::options()
        .write(true)
        .open(&path)
        .unwrap()
        .set_times(std::fs::FileTimes::new().set_modified(
            std::time::SystemTime::now() - std::time::Duration::from_secs(31 * 86400),
        ))
        .unwrap();
    path
}

fn settled_record() -> Vec<u8> {
    let root = private_root();
    emit_retention_event(
        root.path(),
        AuditComponentName::CoshCore,
        &RetentionExecution::default(),
        &AuditSettings::default(),
    )
    .unwrap();
    let (segments, _) = discover_segments(root.path()).unwrap();
    std::fs::read(&segments[0].path).unwrap()
}

#[test]
fn unreadable_lifecycle_does_not_report_complete_or_delete_healthy_expired_segments() {
    let good = settled_record();
    let mut unknown: serde_json::Value = serde_json::from_slice(&good).unwrap();
    unknown["outcome"]["status"] = "unknown".into();
    let mut future = unknown.clone();
    future["event_type"] = "future.lifecycle".into();
    future["outcome"]["status"] = "success".into();
    for input in [
        b"not-json\n".to_vec(),
        format!("{unknown}\n").into_bytes(),
        format!("{future}\n").into_bytes(),
        vec![b'x'; cosh_types::audit::MAX_AUDIT_RECORD_BYTES + 1],
    ] {
        let root = private_root();
        let bad = expired_segment(root.path(), "bad.jsonl", &input);
        let healthy = expired_segment(root.path(), "healthy.jsonl", &good);
        let execution =
            execute_retention(root.path(), &AuditSettings::default(), Utc::now()).unwrap();
        assert_ne!(execution.status, RetentionExecutionStatus::Complete);
        assert!(execution.deleted.is_empty());
        assert_eq!(std::fs::read(bad).unwrap(), input);
        assert_eq!(std::fs::read(healthy).unwrap(), good);
    }
}

#[test]
fn lifecycle_scan_limit_does_not_report_complete_for_1025_valid_small_segments() {
    let root = private_root();
    let good = settled_record();
    for index in 0..1025 {
        expired_segment(root.path(), &format!("segment-{index:04}.jsonl"), &good);
    }
    let execution = execute_retention(root.path(), &AuditSettings::default(), Utc::now()).unwrap();
    assert_eq!(execution.plan.total_bytes, 1025 * good.len() as u64);
    assert_ne!(execution.status, RetentionExecutionStatus::Complete);
    assert!(execution.deleted.is_empty());
    assert_eq!(discover_segments(root.path()).unwrap().0.len(), 1025);
}

#[test]
fn scheduled_blocked_scan_preserves_success_time_and_retries_after_repair() {
    let root = private_root();
    let settings = AuditSettings::default();
    let previous = Utc::now() - Duration::days(2);
    update_state(root.path(), settings.clone(), |state| {
        state.last_retention_success = Some(previous);
    })
    .unwrap();
    let bad = expired_segment(root.path(), "bad.jsonl", b"not-json\n");
    schedule_retention(
        root.path().to_path_buf(),
        settings.clone(),
        AuditComponentName::CoshCore,
    );
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    let state = loop {
        if let Some(state) = read_state(root.path()).unwrap() {
            if state.last_retention_error.is_some()
                || state.last_retention_success != Some(previous)
            {
                break state;
            }
        }
        assert!(
            std::time::Instant::now() < deadline,
            "scheduler did not finish"
        );
        std::thread::yield_now();
    };
    assert_eq!(state.last_retention_success, Some(previous));
    assert_eq!(
        state.last_retention_error.unwrap().code,
        "lifecycle_scan_blocked"
    );
    let (segments, _) = discover_segments(root.path()).unwrap();
    let event: AuditEventV1 = segments
        .iter()
        .filter_map(|segment| serde_json::from_slice(&std::fs::read(&segment.path).unwrap()).ok())
        .find(|event: &AuditEventV1| event.event_type.as_str() == "retention.pruned")
        .unwrap();
    assert_eq!(event.outcome.status, AuditOutcomeStatus::Failed);
    assert!(event.outcome.retryable);
    std::fs::remove_file(bad).unwrap();
    schedule_retention(
        root.path().to_path_buf(),
        settings,
        AuditComponentName::CoshCore,
    );
    loop {
        let state = read_state(root.path()).unwrap().unwrap();
        if state.last_retention_success > Some(previous) {
            assert!(state.last_retention_error.is_none());
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "scheduler did not retry"
        );
        std::thread::yield_now();
    }
}
