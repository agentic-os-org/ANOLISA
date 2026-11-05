use chrono::{Duration, Utc};
use cosh_platform::audit::retention::{execute_retention, plan_retention, plan_retention_dry_run};
use cosh_types::audit::*;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

fn approval(session: &str, terminal: bool) -> Vec<u8> {
    let now = chrono::DateTime::from_timestamp(1_700_000_000 + i64::from(terminal), 0).unwrap();
    let event = AuditEventV1::new(
        uuid::Uuid::new_v4().to_string(),
        if terminal {
            KnownAuditEventType::ApprovalResolved
        } else {
            KnownAuditEventType::ApprovalRequested
        }
        .into(),
        now,
        now,
        0,
        AuditComponent {
            name: AuditComponentName::CoshCore,
            version: env!("CARGO_PKG_VERSION").into(),
        },
        AuditIdentity {
            provider_session_id: Some(session.into()),
            run_id: Some("run-0".into()),
            request_id: Some("req-0".into()),
            ..Default::default()
        },
        AuditActor {
            kind: AuditActorKind::System,
            uid: None,
            euid: None,
        },
        AuditEventOutcome {
            status: if terminal {
                AuditOutcomeStatus::Allowed
            } else {
                AuditOutcomeStatus::Started
            },
            code: None,
            retryable: false,
        },
        AuditSubject {
            kind: "approval".into(),
            name: None,
        },
        &AuditApprovalData::default(),
        AuditRedaction {
            policy_version: "audit-redaction-v1".into(),
            status: AuditRedactionStatus::Clean,
            fields: Vec::new(),
        },
    )
    .unwrap();
    let mut bytes = serde_json::to_vec(&event).unwrap();
    bytes.push(b'\n');
    bytes
}

fn private_segment(root: &Path, name: &str, bytes: &[u8]) -> PathBuf {
    let directory = root.join("v1/segments/2020-01-01");
    fs::create_dir_all(&directory).unwrap();
    for path in [
        root.to_path_buf(),
        root.join("v1"),
        root.join("v1/segments"),
        directory.clone(),
    ] {
        fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
    }
    let path = directory.join(name);
    fs::write(&path, bytes).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
    path
}

#[test]
fn closed_unresolved_approval_survives_while_resolved_control_is_pruned() {
    let root = tempfile::tempdir().unwrap();
    let pending_bytes = approval("pending", false);
    let pending = private_segment(root.path(), "pending.jsonl", &pending_bytes);
    let settled_bytes = [approval("settled", false), approval("settled", true)].concat();
    let settled = private_segment(root.path(), "settled.jsonl", &settled_bytes);
    let result = execute_retention(
        root.path(),
        &AuditSettings::default(),
        Utc::now() + Duration::days(31),
    )
    .unwrap();
    assert_eq!(fs::read(pending).unwrap(), pending_bytes);
    assert!(!settled.exists());
    assert_eq!(result.deleted, ["settled.jsonl"]);
}

#[test]
fn cross_segment_terminal_is_deleted_after_its_predecessor() {
    let root = tempfile::tempdir().unwrap();
    let terminal = private_segment(root.path(), "a-terminal.jsonl", &approval("same", true));
    let start = private_segment(root.path(), "z-start.jsonl", &approval("same", false));
    let result = execute_retention(
        root.path(),
        &AuditSettings::default(),
        Utc::now() + Duration::days(31),
    )
    .unwrap();
    assert!(!start.exists() && !terminal.exists());
    assert_eq!(result.deleted, ["z-start.jsonl", "a-terminal.jsonl"]);
}

#[test]
fn terminal_remains_until_unselected_predecessor_can_be_removed() {
    let root = tempfile::tempdir().unwrap();
    let terminal = private_segment(root.path(), "a-terminal.jsonl", &approval("same", true));
    let start = private_segment(root.path(), "z-start.jsonl", &approval("same", false));
    fs::File::options()
        .write(true)
        .open(&terminal)
        .unwrap()
        .set_times(fs::FileTimes::new().set_modified(
            std::time::SystemTime::now() - std::time::Duration::from_secs(31 * 86400),
        ))
        .unwrap();
    let settings = AuditSettings::default();
    assert!(execute_retention(root.path(), &settings, Utc::now())
        .unwrap()
        .deleted
        .is_empty());
    assert!(start.exists() && terminal.exists());
    assert_eq!(
        execute_retention(root.path(), &settings, Utc::now() + Duration::days(31))
            .unwrap()
            .deleted,
        ["z-start.jsonl", "a-terminal.jsonl"]
    );
}

#[test]
fn request_id_does_not_join_terminal_from_another_session() {
    let root = tempfile::tempdir().unwrap();
    let pending_bytes = approval("pending", false);
    let pending = private_segment(root.path(), "pending.jsonl", &pending_bytes);
    let terminal = private_segment(root.path(), "terminal.jsonl", &approval("other", true));
    let result = execute_retention(
        root.path(),
        &AuditSettings::default(),
        Utc::now() + Duration::days(31),
    )
    .unwrap();
    assert_eq!(fs::read(pending).unwrap(), pending_bytes);
    assert!(!terminal.exists());
    assert_eq!(result.deleted, ["terminal.jsonl"]);
}

#[test]
fn disk_cap_reclaims_settled_evidence_and_preserves_pending_evidence() {
    let root = tempfile::tempdir().unwrap();
    let pending_bytes = approval("pending", false);
    let pending = private_segment(root.path(), "pending.jsonl", &pending_bytes);
    let settled_bytes = [approval("settled", false), approval("settled", true)].concat();
    let settled = private_segment(root.path(), "settled.jsonl", &settled_bytes);
    let settings = AuditSettings {
        max_disk_bytes: settled_bytes.len() as u64,
        ..Default::default()
    };
    let result = execute_retention(root.path(), &settings, Utc::now()).unwrap();
    assert_eq!(fs::read(pending).unwrap(), pending_bytes);
    assert!(!settled.exists());
    assert_eq!(result.plan.selected_bytes, settled_bytes.len() as u64);
}

#[test]
fn orphan_dry_run_matches_execution_without_changing_input() {
    let root = tempfile::tempdir().unwrap();
    let bytes = [approval("settled", false), approval("settled", true)].concat();
    let path = private_segment(root.path(), "orphan.jsonl.active", &bytes);
    let settings = AuditSettings::default();
    let now = Utc::now() + Duration::days(31);
    let plan = plan_retention_dry_run(root.path(), &settings, now).unwrap();
    assert_eq!(plan.candidates.len(), 1);
    assert_eq!(plan.candidates[0].basename, "orphan.jsonl");
    assert_eq!(fs::read(&path).unwrap(), bytes);
    let result = execute_retention(root.path(), &settings, now).unwrap();
    assert_eq!(result.plan, plan);
    assert_eq!(result.deleted, ["orphan.jsonl"]);
    assert!(!path.exists());
}

#[test]
fn unsafe_closed_mode_and_hard_links_are_not_unlinked() {
    let bytes = [approval("settled", false), approval("settled", true)].concat();
    for hard_link in [false, true] {
        let root = tempfile::tempdir().unwrap();
        let path = private_segment(root.path(), "unsafe.jsonl", &bytes);
        let sentinel = root.path().join("outside-sentinel");
        if hard_link {
            fs::hard_link(&path, &sentinel).unwrap();
        } else {
            fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
        }
        let result = execute_retention(
            root.path(),
            &AuditSettings::default(),
            Utc::now() + Duration::days(31),
        )
        .unwrap();
        assert!(result.deleted.is_empty());
        assert_eq!(fs::read(path).unwrap(), bytes);
        if hard_link {
            assert_eq!(fs::read(sentinel).unwrap(), bytes);
        }
    }
}

fn resumed_session_record(terminal: bool, second: i64) -> Vec<u8> {
    let mut event: AuditEventV1 =
        serde_json::from_slice(&approval("resumed-provider", terminal)).unwrap();
    event.event_type = if terminal {
        KnownAuditEventType::SessionEnded
    } else {
        KnownAuditEventType::SessionStarted
    }
    .into();
    event.identity = AuditIdentity {
        provider_session_id: Some("resumed-provider".into()),
        ..Default::default()
    };
    event.occurred_at = chrono::DateTime::from_timestamp(1_700_000_000 + second, 0).unwrap();
    event.observed_at = event.occurred_at;
    event.outcome.status = if terminal {
        AuditOutcomeStatus::Success
    } else {
        AuditOutcomeStatus::Started
    };
    event.subject.kind = "session".into();
    event.validate().unwrap();
    let mut bytes = serde_json::to_vec(&event).unwrap();
    bytes.push(b'\n');
    bytes
}

#[test]
fn historical_session_end_does_not_release_resumed_pending_start() {
    for resumed_name in ["a-resumed.jsonl", "z-resumed.jsonl"] {
        let root = tempfile::tempdir().unwrap();
        private_segment(
            root.path(),
            "m-previous.jsonl",
            &[
                resumed_session_record(false, 1),
                resumed_session_record(true, 2),
            ]
            .concat(),
        );
        let pending_bytes = resumed_session_record(false, 3);
        let pending = private_segment(root.path(), resumed_name, &pending_bytes);
        let result = execute_retention(
            root.path(),
            &AuditSettings::default(),
            Utc::now() + Duration::days(31),
        )
        .unwrap();
        assert!(
            pending.exists(),
            "previous session end released resumed pending evidence: {result:?}"
        );
        assert_eq!(fs::read(pending).unwrap(), pending_bytes);
        assert!(!result.deleted.iter().any(|name| name == resumed_name));
    }
}

#[test]
fn resumed_session_can_be_pruned_after_its_own_end() {
    let root = tempfile::tempdir().unwrap();
    let previous = private_segment(
        root.path(),
        "previous.jsonl",
        &[
            resumed_session_record(false, 1),
            resumed_session_record(true, 2),
        ]
        .concat(),
    );
    let resumed = private_segment(
        root.path(),
        "resumed.jsonl",
        &[
            resumed_session_record(false, 3),
            resumed_session_record(true, 4),
        ]
        .concat(),
    );
    let result = execute_retention(
        root.path(),
        &AuditSettings::default(),
        Utc::now() + Duration::days(31),
    )
    .unwrap();
    assert!(!previous.exists() && !resumed.exists(), "{result:?}");
    assert_eq!(result.deleted.len(), 2);
}

#[test]
fn resumed_end_does_not_settle_an_earlier_unclosed_start() {
    let root = tempfile::tempdir().unwrap();
    let pending_bytes = resumed_session_record(false, 1);
    let pending = private_segment(root.path(), "previous.jsonl", &pending_bytes);
    private_segment(
        root.path(),
        "resumed.jsonl",
        &[
            resumed_session_record(false, 2),
            resumed_session_record(true, 3),
        ]
        .concat(),
    );
    let result = execute_retention(
        root.path(),
        &AuditSettings::default(),
        Utc::now() + Duration::days(31),
    )
    .unwrap();
    assert!(
        pending.exists(),
        "ambiguous repeated start was settled: {result:?}"
    );
    assert_eq!(fs::read(pending).unwrap(), pending_bytes);
}

#[test]
fn equal_time_across_segments_does_not_release_pending_evidence() {
    let root = tempfile::tempdir().unwrap();
    private_segment(
        root.path(),
        "previous.jsonl",
        &[
            resumed_session_record(false, 1),
            resumed_session_record(true, 3),
        ]
        .concat(),
    );
    let pending_bytes = resumed_session_record(false, 3);
    let pending = private_segment(root.path(), "resumed.jsonl", &pending_bytes);
    let result = execute_retention(
        root.path(),
        &AuditSettings::default(),
        Utc::now() + Duration::days(31),
    )
    .unwrap();
    assert!(
        pending.exists(),
        "cross-segment timestamp tie was guessed: {result:?}"
    );
    assert_eq!(fs::read(pending).unwrap(), pending_bytes);
}

#[test]
fn decreasing_time_inside_segment_does_not_release_pending_evidence() {
    let root = tempfile::tempdir().unwrap();
    let bytes = [
        resumed_session_record(false, 2),
        resumed_session_record(true, 1),
    ]
    .concat();
    let path = private_segment(root.path(), "regressed.jsonl", &bytes);
    let result = execute_retention(
        root.path(),
        &AuditSettings::default(),
        Utc::now() + Duration::days(31),
    )
    .unwrap();
    assert!(
        path.exists(),
        "decreasing writer time was trusted: {result:?}"
    );
    assert_eq!(fs::read(path).unwrap(), bytes);
}

#[test]
fn same_segment_row_order_resolves_equal_writer_times() {
    let root = tempfile::tempdir().unwrap();
    let path = private_segment(
        root.path(),
        "settled.jsonl",
        &[
            resumed_session_record(false, 1),
            resumed_session_record(true, 1),
        ]
        .concat(),
    );
    let result = execute_retention(
        root.path(),
        &AuditSettings::default(),
        Utc::now() + Duration::days(31),
    )
    .unwrap();
    assert!(!path.exists(), "{result:?}");
    assert_eq!(result.deleted, ["settled.jsonl"]);
}

#[test]
fn tool_requested_and_execution_started_share_their_terminal() {
    let root = tempfile::tempdir().unwrap();
    for (index, kind) in [
        KnownAuditEventType::ToolRequested,
        KnownAuditEventType::ToolExecutionStarted,
        KnownAuditEventType::ToolCompleted,
    ]
    .into_iter()
    .enumerate()
    {
        let terminal = kind == KnownAuditEventType::ToolCompleted;
        let mut value: serde_json::Value =
            serde_json::from_slice(&resumed_session_record(terminal, index as i64)).unwrap();
        value["event_type"] = kind.as_str().into();
        value["identity"] = serde_json::json!({"run_id": "run-0", "tool_use_id": "tool-0"});
        value["data"] = serde_json::json!({"tool_kind": "shell"});
        value["subject"]["kind"] = "tool".into();
        let event: AuditEventV1 = serde_json::from_value(value).unwrap();
        event.validate().unwrap();
        private_segment(
            root.path(),
            &format!("{index}.jsonl"),
            format!("{}\n", serde_json::to_string(&event).unwrap()).as_bytes(),
        );
    }
    let result = execute_retention(
        root.path(),
        &AuditSettings::default(),
        Utc::now() + Duration::days(31),
    )
    .unwrap();
    assert_eq!(result.deleted, ["0.jsonl", "1.jsonl", "2.jsonl"]);
}

#[test]
fn unpaired_old_session_end_cannot_settle_pending_start_after_clock_rollback() {
    let root = tempfile::tempdir().unwrap();
    let old_terminal_bytes = resumed_session_record(true, 5);
    let old_terminal = private_segment(root.path(), "previous.jsonl", &old_terminal_bytes);
    let pending_bytes = resumed_session_record(false, 4);
    let pending = private_segment(root.path(), "resumed.jsonl", &pending_bytes);
    let result = execute_retention(
        root.path(),
        &AuditSettings::default(),
        Utc::now() + Duration::days(31),
    )
    .unwrap();
    assert!(
        pending.exists(),
        "clock rollback released resumed pending evidence: {result:?}"
    );
    assert_eq!(fs::read(pending).unwrap(), pending_bytes);
    assert_eq!(fs::read(old_terminal).unwrap(), old_terminal_bytes);
    assert!(result.deleted.is_empty());
}

#[test]
fn cross_segment_session_pair_is_preserved_without_invocation_identity() {
    let root = tempfile::tempdir().unwrap();
    let start_bytes = resumed_session_record(false, 1);
    let start = private_segment(root.path(), "start.jsonl", &start_bytes);
    let terminal_bytes = resumed_session_record(true, 2);
    let terminal = private_segment(root.path(), "terminal.jsonl", &terminal_bytes);
    let result = execute_retention(
        root.path(),
        &AuditSettings::default(),
        Utc::now() + Duration::days(31),
    )
    .unwrap();
    assert!(
        start.exists() && terminal.exists(),
        "cross-segment session generation was inferred: {result:?}"
    );
    assert_eq!(fs::read(start).unwrap(), start_bytes);
    assert_eq!(fs::read(terminal).unwrap(), terminal_bytes);
    assert!(result.deleted.is_empty());
}

#[test]
fn unreadable_date_directory_prevents_retention_success_and_deletion() {
    assert_ne!(
        nix::unistd::geteuid().as_raw(),
        0,
        "run as an unprivileged actor"
    );
    let root = tempfile::tempdir().unwrap();
    let settled = [approval("settled", false), approval("settled", true)].concat();
    let visible = private_segment(root.path(), "visible.jsonl", &settled);
    let hidden = root.path().join("v1/segments/2019-01-01");
    fs::create_dir(&hidden).unwrap();
    fs::set_permissions(&hidden, fs::Permissions::from_mode(0o000)).unwrap();
    assert_eq!(
        fs::read_dir(&hidden).unwrap_err().kind(),
        std::io::ErrorKind::PermissionDenied
    );
    let now = Utc::now() + Duration::days(31);
    let result = execute_retention(root.path(), &AuditSettings::default(), now);
    let planned = plan_retention(root.path(), &AuditSettings::default(), now);
    let dry_run = plan_retention_dry_run(root.path(), &AuditSettings::default(), now);
    fs::set_permissions(&hidden, fs::Permissions::from_mode(0o700)).unwrap();

    for result in [result.map(|_| ()), planned.map(|_| ()), dry_run.map(|_| ())] {
        assert_eq!(
            result.unwrap_err().code,
            cosh_types::error::ErrorCode::AuditLogError
        );
    }
    assert_eq!(fs::read(visible).unwrap(), settled);
}

#[test]
fn planning_refuses_unreadable_lifecycle_evidence() {
    let mut unknown: serde_json::Value =
        serde_json::from_slice(&approval("unknown", false)).unwrap();
    unknown["event_type"] = "future.lifecycle".into();
    for input in [b"not-json\n".to_vec(), format!("{unknown}\n").into_bytes()] {
        let root = tempfile::tempdir().unwrap();
        let bad = private_segment(root.path(), "bad.jsonl", &input);
        for result in [
            plan_retention(root.path(), &AuditSettings::default(), Utc::now()),
            plan_retention_dry_run(root.path(), &AuditSettings::default(), Utc::now()),
        ] {
            let error = result.expect_err("planning must expose a blocked lifecycle scan");
            assert_eq!(error.code, cosh_types::error::ErrorCode::AuditLogError);
            assert!(error.message.contains("lifecycle_scan_blocked"));
        }
        assert_eq!(fs::read(bad).unwrap(), input);
    }
}

#[test]
fn planning_refuses_1025_segments_but_accepts_an_empty_tree() {
    let root = tempfile::tempdir().unwrap();
    for result in [
        plan_retention(root.path(), &AuditSettings::default(), Utc::now()),
        plan_retention_dry_run(root.path(), &AuditSettings::default(), Utc::now()),
    ] {
        assert!(result.unwrap().candidates.is_empty());
    }
    let settled = [approval("settled", false), approval("settled", true)].concat();
    for index in 0..1025 {
        private_segment(root.path(), &format!("segment-{index:04}.jsonl"), &settled);
    }
    for result in [
        plan_retention(root.path(), &AuditSettings::default(), Utc::now()),
        plan_retention_dry_run(root.path(), &AuditSettings::default(), Utc::now()),
    ] {
        let error = result.expect_err("planning must expose a bounded lifecycle scan");
        assert_eq!(error.code, cosh_types::error::ErrorCode::AuditLogError);
        assert!(error.message.contains("lifecycle_scan_blocked"));
    }
    assert_eq!(
        fs::read_dir(root.path().join("v1/segments/2020-01-01"))
            .unwrap()
            .count(),
        1025
    );
}
