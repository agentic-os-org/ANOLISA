use super::*;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

use crate::types::audit::AUDIT_REDACTION_POLICY_VERSION;

fn private_root() -> PathBuf {
    let root = std::env::temp_dir().join(format!(
        "cosh-shell-audit-test-{}-{}",
        std::process::id(),
        uuid::Uuid::new_v4()
    ));
    std::fs::create_dir(&root).unwrap();
    #[cfg(unix)]
    std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).unwrap();
    root.canonicalize().unwrap()
}

#[test]
fn distinct_writers_lock_distinct_segments_and_close() {
    let root = private_root();
    let mut first = AuditSegmentWriter::create(&root).unwrap();
    let mut second = AuditSegmentWriter::create(&root).unwrap();
    let event = || {
        AuditEventV1::shell(
            "session.started",
            AuditIdentity {
                shell_session_id: Some("session".to_string()),
                ..AuditIdentity::default()
            },
            AuditEventOutcome {
                status: AuditOutcomeStatus::Started,
                code: None,
                retryable: false,
            },
            AuditSubject {
                kind: "session".to_string(),
                name: None,
            },
            &serde_json::json!({}),
            AuditRedaction::clean(),
        )
        .unwrap()
    };
    first.append(&mut event(), true).unwrap();
    second.append(&mut event(), true).unwrap();
    assert_ne!(first.active_path(), second.active_path());
    first.close().unwrap();
    second.close().unwrap();
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn command_projection_contains_no_raw_command_cwd_or_path() {
    let root = private_root();
    let mut recorder = ShellAuditRecorder {
        writer: Some(AuditSegmentWriter::create(&root).unwrap()),
        writer_root: Some(root.clone()),
        mode: AuditMode::BestEffort,
        max_disk_bytes: crate::config::audit::AuditSettings::default().max_disk_bytes,
        shell_session_id: "session-1".to_string(),
        seen_events: 0,
        truncation: TruncationReport::Idle,
        hash_salt: "salt".to_string(),
        degraded: false,
        warning_emitted: false,
        owned_approvals: std::collections::HashSet::new(),
        resolved_approvals: std::collections::HashSet::new(),
        command_refs: std::collections::HashMap::new(),
    };
    let secret = "super-secret-command-value";
    let mut started = ShellEvent::command_started(
        "session-1",
        "cmd-1",
        format!("curl --token {secret}"),
        "/private/secret/work",
        1,
    );
    started.terminal_output_ref = Some("/private/secret/output".to_string());
    recorder.observe_shell_events(&[started]);
    drop(recorder);
    let content = walk_segment_text(&root);
    assert!(!content.contains(secret), "{content}");
    assert!(!content.contains("/private/secret"), "{content}");
    assert!(content.contains("terminal-output://") || content.contains("session.started"));
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn required_owned_approval_resolution_fails_before_execution_boundary() {
    let mut recorder = ShellAuditRecorder {
        writer: None,
        writer_root: None,
        mode: AuditMode::Required,
        max_disk_bytes: crate::config::audit::AuditSettings::default().max_disk_bytes,
        shell_session_id: "session-1".to_string(),
        seen_events: 0,
        truncation: TruncationReport::Idle,
        hash_salt: "salt".to_string(),
        degraded: true,
        warning_emitted: false,
        owned_approvals: std::collections::HashSet::new(),
        resolved_approvals: std::collections::HashSet::new(),
        command_refs: std::collections::HashMap::new(),
    };
    let requested = recorder.record_approval_requested(ShellApprovalAuditInput {
        id: "approval-1",
        audit_ref: None,
        session_id: "session-1",
        run_id: "run-1",
        request_id: None,
        tool_use_id: None,
        subject: "shell command",
        risk: "medium",
        assessment: None,
        preview: "$ echo ok",
        status: "pending",
    });
    assert!(requested.is_none());
    let result = recorder.record_approval_resolved(ShellApprovalAuditInput {
        id: "approval-1",
        audit_ref: None,
        session_id: "session-1",
        run_id: "run-1",
        request_id: None,
        tool_use_id: None,
        subject: "shell command",
        risk: "medium",
        assessment: None,
        preview: "$ echo ok",
        status: "approved",
    });
    assert!(result
        .unwrap_err()
        .starts_with("AUDIT_REQUIRED_UNAVAILABLE"));
}

#[test]
fn approval_drop_audit_distinguishes_drain_from_user_denial() {
    let root = private_root();
    let mut recorder = ShellAuditRecorder::test_with_root(&root);

    let event_id = recorder.record_approval_dropped("run-1", "ctrl-9", "batch_drain");
    assert!(event_id.is_some());
    drop(recorder);

    let content = walk_segment_text(&root);
    let event: serde_json::Value =
        serde_json::from_str(content.lines().next().expect("audit record")).expect("audit json");
    assert_eq!(event["event_type"], "approval.dropped");
    assert_eq!(event["identity"]["shell_session_id"], "audit-test-session");
    assert_eq!(event["identity"]["run_id"], "run-1");
    assert_eq!(event["identity"]["request_id"], "ctrl-9");
    assert_eq!(event["outcome"]["status"], "cancelled");
    assert_eq!(event["subject"]["kind"], "approval");
    assert_eq!(event["data"]["decision"], "dropped");
    assert_eq!(event["data"]["reason_code"], "batch_drain");
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn required_core_host_execution_fails_before_handoff_boundary() {
    let mut recorder = ShellAuditRecorder {
        writer: None,
        writer_root: None,
        mode: AuditMode::Required,
        max_disk_bytes: crate::config::audit::AuditSettings::default().max_disk_bytes,
        shell_session_id: "session-1".to_string(),
        seen_events: 0,
        truncation: TruncationReport::Idle,
        hash_salt: "salt".to_string(),
        degraded: true,
        warning_emitted: false,
        owned_approvals: std::collections::HashSet::new(),
        resolved_approvals: std::collections::HashSet::new(),
        command_refs: std::collections::HashMap::new(),
    };

    let result = recorder.authorize_host_execution(
        ShellApprovalAuditInput {
            id: "approval-1",
            audit_ref: None,
            session_id: "session-1",
            run_id: "run-1",
            request_id: Some("request-1"),
            tool_use_id: Some("tool-1"),
            subject: "run_shell_command",
            risk: "medium",
            assessment: None,
            preview: "$ echo ok",
            status: "approved",
        },
        "shell_foreground_handoff",
    );

    assert!(result
        .unwrap_err()
        .starts_with("AUDIT_REQUIRED_UNAVAILABLE"));
}

#[test]
fn core_host_execution_does_not_duplicate_the_approval_resolution() {
    let root = private_root();
    let mut recorder = ShellAuditRecorder {
        writer: Some(AuditSegmentWriter::create(&root).unwrap()),
        writer_root: Some(root.clone()),
        mode: AuditMode::BestEffort,
        max_disk_bytes: crate::config::audit::AuditSettings::default().max_disk_bytes,
        shell_session_id: "session-1".to_string(),
        seen_events: 0,
        truncation: TruncationReport::Idle,
        hash_salt: "salt".to_string(),
        degraded: false,
        warning_emitted: false,
        owned_approvals: std::collections::HashSet::new(),
        resolved_approvals: std::collections::HashSet::new(),
        command_refs: std::collections::HashMap::new(),
    };

    recorder
        .authorize_host_execution(
            ShellApprovalAuditInput {
                id: "approval-1",
                audit_ref: Some("core-approval-event"),
                session_id: "session-1",
                run_id: "run-1",
                request_id: Some("request-1"),
                tool_use_id: Some("tool-1"),
                subject: "run_shell_command",
                risk: "medium",
                assessment: None,
                preview: "$ echo ok",
                status: "approved",
            },
            "shell_foreground_handoff",
        )
        .unwrap();

    drop(recorder);
    let content = walk_segment_text(&root);
    assert!(content.contains("\"event_type\":\"tool.execution.started\""));
    assert!(!content.contains("\"event_type\":\"approval.resolved\""));
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn shell_owned_approval_does_not_require_a_provider_tool_identity() {
    let mut recorder = ShellAuditRecorder {
        writer: None,
        writer_root: None,
        mode: AuditMode::Required,
        max_disk_bytes: crate::config::audit::AuditSettings::default().max_disk_bytes,
        shell_session_id: "session-1".to_string(),
        seen_events: 0,
        truncation: TruncationReport::Idle,
        hash_salt: "salt".to_string(),
        degraded: false,
        warning_emitted: false,
        owned_approvals: std::collections::HashSet::from(["approval-1".to_string()]),
        resolved_approvals: std::collections::HashSet::new(),
        command_refs: std::collections::HashMap::new(),
    };

    let result = recorder.authorize_host_execution(
        ShellApprovalAuditInput {
            id: "approval-1",
            audit_ref: None,
            session_id: "session-1",
            run_id: "run-1",
            request_id: None,
            tool_use_id: None,
            subject: "Bash",
            risk: "medium",
            assessment: None,
            preview: "$ echo ok",
            status: "approved",
        },
        "shell_foreground_handoff",
    );

    assert!(result.is_ok());
}

#[test]
fn successful_write_closes_shell_degraded_episode() {
    let root = private_root();
    let mut recorder = ShellAuditRecorder {
        writer: Some(AuditSegmentWriter::create(&root).unwrap()),
        writer_root: Some(root.clone()),
        mode: AuditMode::BestEffort,
        max_disk_bytes: crate::config::audit::AuditSettings::default().max_disk_bytes,
        shell_session_id: "session-1".to_string(),
        seen_events: 0,
        truncation: TruncationReport::Idle,
        hash_salt: "salt".to_string(),
        degraded: true,
        warning_emitted: true,
        owned_approvals: std::collections::HashSet::new(),
        resolved_approvals: std::collections::HashSet::new(),
        command_refs: std::collections::HashMap::new(),
    };
    assert!(recorder
        .record_evidence_accessed("command_output", Some("small"), None, true)
        .is_some());
    assert!(!recorder.degraded);
    drop(recorder);
    let content = walk_segment_text(&root);
    assert!(content.contains("\"event_type\":\"audit.degraded\""));
    assert!(content.contains("\"event_type\":\"audit.recovered\""));
    let _ = std::fs::remove_dir_all(root);
}

/// Serializes the shrink tests: they share the warning callsite, and the
/// thread-scoped capture subscriber can miss it when a concurrent test
/// evaluates the same callsite without any subscriber installed.
fn shrink_guard() -> std::sync::MutexGuard<'static, ()> {
    static SHRINK_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    SHRINK_LOCK
        .lock()
        .unwrap_or_else(|poison| poison.into_inner())
}

#[test]
fn observe_shell_events_shrink_does_not_replay_recorded_commands() {
    let _guard = shrink_guard();
    let root = private_root();
    let mut recorder = recording_recorder(&root);
    let started = ShellEvent::command_started("session-1", "cmd-1", "ls -l", "/tmp/work", 1);
    let mut completed = started.clone();
    completed.kind = ShellEventKind::CommandCompleted;
    completed.exit_code = Some(0);
    recorder.observe_shell_events(&[started.clone(), completed]);
    recorder.observe_shell_events(std::slice::from_ref(&started));
    // A second shrunken batch in the same episode must not add another marker.
    recorder.observe_shell_events(&[started]);
    drop(recorder);

    assert_eq!(
        record_count(&root, "shell.command.started", None),
        1,
        "a shrunken event snapshot must not replay already-recorded commands"
    );
    assert_eq!(
        record_count(&root, "audit.truncated", None),
        1,
        "the projection gap must be persisted exactly once per shrink episode"
    );
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn observe_shell_events_resumes_projection_after_a_shrink() {
    let _guard = shrink_guard();
    let root = private_root();
    let mut recorder = recording_recorder(&root);
    let first = ShellEvent::command_started("session-1", "cmd-1", "ls -l", "/tmp/work", 1);
    let mut completed = first.clone();
    completed.kind = ShellEventKind::CommandCompleted;
    completed.exit_code = Some(0);
    recorder.observe_shell_events(&[first.clone(), completed.clone()]);
    // The transient shrunken view is skipped without moving the projected
    // high-water mark, so the restored snapshot projects only the events
    // past the mark and never re-records the persisted completion.
    recorder.observe_shell_events(std::slice::from_ref(&first));
    let second = ShellEvent::command_started("session-1", "cmd-2", "pwd", "/tmp/work", 2);
    recorder.observe_shell_events(&[first.clone(), completed.clone(), second.clone()]);
    // Advancing past the mark closes the episode; a later shrink is a new
    // one and persists its own gap marker.
    recorder.observe_shell_events(std::slice::from_ref(&first));
    drop(recorder);

    assert_eq!(record_count(&root, "shell.command.completed", None), 1);
    assert_eq!(record_count(&root, "shell.command.started", None), 2);
    assert_eq!(
        record_count(&root, "shell.command.started", Some("cmd-2")),
        1
    );
    assert_eq!(
        record_count(&root, "audit.truncated", None),
        2,
        "each shrink episode persists its own gap marker"
    );
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn observe_shell_events_retries_the_truncation_marker_until_durable() {
    let _guard = shrink_guard();
    let root = private_root();
    let mut recorder = recording_recorder(&root);
    let started = ShellEvent::command_started("session-1", "cmd-1", "ls -l", "/tmp/work", 1);
    let mut completed = started.clone();
    completed.kind = ShellEventKind::CommandCompleted;
    completed.exit_code = Some(0);
    recorder.observe_shell_events(&[started.clone(), completed]);
    // The first shrunken snapshot hits an unavailable writer: the marker is
    // not durable, so the episode must stay unreported.
    recorder.writer = None;
    let saved_root = recorder.writer_root.take();
    recorder.observe_shell_events(std::slice::from_ref(&started));
    // The writer recovers while the same episode keeps shrinking: the next
    // observation retries and persists the marker exactly once.
    recorder.writer_root = saved_root;
    recorder.observe_shell_events(std::slice::from_ref(&started));
    recorder.observe_shell_events(&[started]);
    drop(recorder);

    assert_eq!(
        record_count(&root, "audit.truncated", None),
        1,
        "the gap marker must be retried until durable, then stay bounded"
    );
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn observe_shell_events_retries_pending_marker_before_direct_growth() {
    let _guard = shrink_guard();
    let root = private_root();
    let mut recorder = recording_recorder(&root);
    let first = ShellEvent::command_started("session-1", "cmd-1", "ls -l", "/tmp/work", 1);
    let mut completed = first.clone();
    completed.kind = ShellEventKind::CommandCompleted;
    completed.exit_code = Some(0);
    recorder.observe_shell_events(&[first.clone(), completed.clone()]);

    recorder.writer = None;
    let saved_root = recorder.writer_root.take();
    recorder.observe_shell_events(std::slice::from_ref(&first));

    recorder.writer_root = saved_root;
    let second = ShellEvent::command_started("session-1", "cmd-2", "pwd", "/tmp/work", 2);
    recorder.observe_shell_events(&[first, completed, second]);
    assert_eq!(
        recorder.truncation,
        TruncationReport::Idle,
        "durable reporting plus projection growth must re-arm the next episode"
    );
    drop(recorder);

    assert_eq!(
        record_count(&root, "audit.truncated", None),
        1,
        "direct growth must retry the pending gap marker before re-arming"
    );
    assert_eq!(
        record_count(&root, "shell.command.started", Some("cmd-2")),
        1,
        "retrying the marker must not block projection after the high-water mark"
    );
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn observe_shell_events_retries_pending_marker_on_drop() {
    let _guard = shrink_guard();
    let root = private_root();
    let mut recorder = recording_recorder(&root);
    let started = ShellEvent::command_started("session-1", "cmd-1", "ls -l", "/tmp/work", 1);
    let mut completed = started.clone();
    completed.kind = ShellEventKind::CommandCompleted;
    completed.exit_code = Some(0);
    recorder.observe_shell_events(&[started.clone(), completed]);

    recorder.writer = None;
    let saved_root = recorder.writer_root.take();
    recorder.observe_shell_events(&[started]);
    recorder.writer_root = saved_root;
    drop(recorder);

    assert_eq!(
        record_count(&root, "audit.truncated", None),
        1,
        "session shutdown must retry a marker pending outside the segment"
    );
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn observe_shell_events_retries_pending_marker_before_drop_recovery() {
    let _guard = shrink_guard();
    let root = private_root();
    let mut recorder = recording_recorder(&root);
    let started = ShellEvent::command_started("session-1", "cmd-1", "ls -l", "/tmp/work", 1);
    let mut completed = started.clone();
    completed.kind = ShellEventKind::CommandCompleted;
    completed.exit_code = Some(0);
    recorder.observe_shell_events(&[started.clone(), completed]);
    recorder
        .writer
        .as_mut()
        .expect("writer")
        .fail_next_stages(1);
    recorder.observe_shell_events(&[started]);
    drop(recorder);

    let records = walk_segment_text(&root)
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| serde_json::from_str::<serde_json::Value>(line).expect("audit record"))
        .collect::<Vec<_>>();
    let truncated = records
        .iter()
        .position(|record| record["event_type"] == "audit.truncated")
        .expect("pending marker must be retried before shutdown recovery");
    let recovered = records
        .iter()
        .position(|record| record["event_type"] == "audit.recovered")
        .expect("successful shutdown write closes the degraded episode");
    assert!(
        truncated < recovered,
        "the stream must not claim recovery while the gap marker is still undelivered"
    );
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn pending_marker_retry_failure_blocks_following_append_until_delivered() {
    let _guard = shrink_guard();
    let root = private_root();
    let mut recorder = recording_recorder(&root);
    let started = ShellEvent::command_started("session-1", "cmd-1", "ls -l", "/tmp/work", 1);
    let mut completed = started.clone();
    completed.kind = ShellEventKind::CommandCompleted;
    completed.exit_code = Some(0);
    recorder.observe_shell_events(&[started.clone(), completed]);
    recorder
        .writer
        .as_mut()
        .expect("writer")
        .fail_next_stages(2);
    recorder.observe_shell_events(&[started]);

    assert!(
        recorder
            .record_evidence_accessed("command_output", None, None, true)
            .is_none(),
        "an undelivered marker must block a later append from claiming recovery"
    );
    let blocked = walk_segment_text(&root);
    assert!(!blocked.contains("\"event_type\":\"evidence.accessed\""));
    assert!(!blocked.contains("\"event_type\":\"audit.recovered\""));

    assert!(
        recorder
            .record_evidence_accessed("command_output", None, None, true)
            .is_some(),
        "a later append can continue after the marker is delivered"
    );
    drop(recorder);
    let records = walk_segment_text(&root);
    let marker = records.find("\"event_type\":\"audit.truncated\"").unwrap();
    let evidence = records
        .find("\"event_type\":\"evidence.accessed\"")
        .unwrap();
    let recovered = records.find("\"event_type\":\"audit.recovered\"").unwrap();
    assert!(marker < evidence && evidence < recovered);
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn required_approval_fails_closed_while_marker_retry_is_pending() {
    let _guard = shrink_guard();
    let root = private_root();
    let mut recorder = recording_recorder(&root);
    recorder.mode = AuditMode::Required;
    let started = ShellEvent::command_started("session-1", "cmd-1", "ls -l", "/tmp/work", 1);
    let mut completed = started.clone();
    completed.kind = ShellEventKind::CommandCompleted;
    completed.exit_code = Some(0);
    recorder.observe_shell_events(&[started.clone(), completed]);
    recorder
        .writer
        .as_mut()
        .expect("writer")
        .fail_next_stages(2);
    recorder.observe_shell_events(&[started]);

    let request = || ShellApprovalAuditInput {
        id: "approval-1",
        audit_ref: Some("core-approval-event"),
        session_id: "session-1",
        run_id: "run-1",
        request_id: Some("request-1"),
        tool_use_id: Some("tool-1"),
        subject: "run_shell_command",
        risk: "medium",
        assessment: None,
        preview: "$ echo ok",
        status: "approved",
    };
    let error = recorder.record_approval_resolved(request()).unwrap_err();
    assert!(error.starts_with("AUDIT_REQUIRED_UNAVAILABLE:"), "{error}");
    let blocked = walk_segment_text(&root);
    assert!(!blocked.contains("\"event_type\":\"approval.resolved\""));
    assert!(!blocked.contains("\"event_type\":\"audit.recovered\""));

    assert!(recorder
        .record_approval_resolved(request())
        .unwrap()
        .is_some());
    drop(recorder);
    let records = walk_segment_text(&root);
    let marker = records.find("\"event_type\":\"audit.truncated\"").unwrap();
    let approval = records
        .find("\"event_type\":\"approval.resolved\"")
        .unwrap();
    assert!(marker < approval);
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn unsynced_marker_retry_failure_blocks_following_append_until_committed() {
    let _guard = shrink_guard();
    let root = private_root();
    let mut recorder = recording_recorder(&root);
    let started = ShellEvent::command_started("session-1", "cmd-1", "ls -l", "/tmp/work", 1);
    let mut completed = started.clone();
    completed.kind = ShellEventKind::CommandCompleted;
    completed.exit_code = Some(0);
    recorder.observe_shell_events(&[started.clone(), completed]);
    recorder.writer.as_mut().expect("writer").fail_next_syncs(2);
    recorder.observe_shell_events(&[started]);

    assert!(recorder
        .record_evidence_accessed("command_output", None, None, true)
        .is_none());
    assert!(matches!(recorder.truncation, TruncationReport::Unsynced(_)));
    assert!(!walk_segment_text(&root).contains("\"event_type\":\"evidence.accessed\""));

    assert!(recorder
        .record_evidence_accessed("command_output", None, None, true)
        .is_some());
    assert!(matches!(recorder.truncation, TruncationReport::Reported(_)));
    drop(recorder);
    assert_eq!(record_count(&root, "audit.truncated", None), 1);
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn approval_resolution_retries_a_pending_marker_first() {
    let _guard = shrink_guard();
    let root = private_root();
    let mut recorder = recording_recorder(&root);
    let started = ShellEvent::command_started("session-1", "cmd-1", "ls -l", "/tmp/work", 1);
    let mut completed = started.clone();
    completed.kind = ShellEventKind::CommandCompleted;
    completed.exit_code = Some(0);
    recorder.observe_shell_events(&[started.clone(), completed]);
    recorder
        .writer
        .as_mut()
        .expect("writer")
        .fail_next_stages(1);
    recorder.observe_shell_events(&[started]);

    recorder
        .record_approval_resolved(ShellApprovalAuditInput {
            id: "approval-1",
            audit_ref: Some("core-approval-event"),
            session_id: "session-1",
            run_id: "run-1",
            request_id: Some("request-1"),
            tool_use_id: Some("tool-1"),
            subject: "run_shell_command",
            risk: "medium",
            assessment: None,
            preview: "$ echo ok",
            status: "approved",
        })
        .unwrap();
    drop(recorder);

    let types = walk_segment_text(&root)
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| serde_json::from_str::<serde_json::Value>(line).expect("audit record"))
        .map(|record| {
            record["event_type"]
                .as_str()
                .unwrap_or_default()
                .to_string()
        })
        .collect::<Vec<_>>();
    let marker = types.iter().position(|kind| kind == "audit.truncated");
    let approval = types.iter().position(|kind| kind == "approval.resolved");
    assert!(
        marker.is_some() && marker < approval,
        "a direct approval write must deliver the pending gap marker first: {types:?}"
    );
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn writer_keeps_active_segment_after_close_sync_failure() {
    let root = private_root();
    let mut writer = AuditSegmentWriter::create(&root).unwrap();
    let mut event = AuditEventV1::shell(
        "session.started",
        AuditIdentity {
            shell_session_id: Some("session".to_string()),
            ..AuditIdentity::default()
        },
        AuditEventOutcome {
            status: AuditOutcomeStatus::Started,
            code: None,
            retryable: false,
        },
        AuditSubject {
            kind: "session".to_string(),
            name: None,
        },
        &serde_json::json!({}),
        AuditRedaction::clean(),
    )
    .unwrap();
    writer.stage(&mut event).unwrap();
    writer.fail_next_syncs(1);

    assert!(writer.close().is_err());
    assert!(
        writer.active_path().is_some(),
        "a failed close must keep the unsynced segment available for retry"
    );
    assert!(writer.commit(true).is_ok());
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn writer_rejects_commit_without_an_active_segment() {
    let root = private_root();
    let mut writer = AuditSegmentWriter::create(&root).unwrap();

    assert!(
        writer.commit(true).is_err(),
        "an empty writer cannot confirm durability for staged records"
    );
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn observe_shell_events_retries_only_the_sync_of_an_unsynced_marker() {
    let _guard = shrink_guard();
    let root = private_root();
    let mut recorder = recording_recorder(&root);
    let started = ShellEvent::command_started("session-1", "cmd-1", "ls -l", "/tmp/work", 1);
    let mut completed = started.clone();
    completed.kind = ShellEventKind::CommandCompleted;
    completed.exit_code = Some(0);
    recorder.observe_shell_events(&[started.clone(), completed]);
    // The marker is written and flushed but its sync fails twice: it is
    // already in the segment, so the episode must retry only durability.
    recorder.writer.as_mut().expect("writer").fail_next_syncs(2);
    recorder.observe_shell_events(std::slice::from_ref(&started));
    assert!(matches!(recorder.truncation, TruncationReport::Unsynced(_)));
    recorder.observe_shell_events(std::slice::from_ref(&started));
    assert!(
        matches!(recorder.truncation, TruncationReport::Unsynced(_)),
        "the retry must attempt the sync rather than assume durability"
    );
    recorder.observe_shell_events(std::slice::from_ref(&started));
    assert!(matches!(recorder.truncation, TruncationReport::Reported(_)));
    recorder.observe_shell_events(&[started]);
    drop(recorder);

    assert_eq!(
        record_count(&root, "audit.truncated", None),
        1,
        "a sync retry must not append a second marker for the same episode"
    );
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn observe_shell_events_truncation_marker_matches_the_canonical_fixture() {
    let _guard = shrink_guard();
    let fixture: serde_json::Value = serde_json::from_str(include_str!(
        "../../../cosh-types/tests/fixtures/audit-v1-shell-truncated.json"
    ))
    .expect("canonical truncation fixture");
    let root = private_root();
    let mut recorder = recording_recorder(&root);
    let started = ShellEvent::command_started("session-1", "cmd-1", "ls -l", "/tmp/work", 1);
    let mut completed = started.clone();
    completed.kind = ShellEventKind::CommandCompleted;
    completed.exit_code = Some(0);
    recorder.observe_shell_events(&[started.clone(), completed]);
    recorder.observe_shell_events(&[started]);
    drop(recorder);

    let marker = walk_segment_text(&root)
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| serde_json::from_str::<serde_json::Value>(line).expect("audit record"))
        .find(|record| record["event_type"] == "audit.truncated")
        .expect("persisted truncation marker");
    for field in [
        "schema",
        "schema_version",
        "event_type",
        "identity",
        "actor",
        "outcome",
        "subject",
        "data",
        "redaction",
    ] {
        assert_eq!(
            marker[field], fixture[field],
            "the producer drifted from the canonical fixture at `{field}`"
        );
    }
    assert_eq!(marker["component"]["name"], fixture["component"]["name"]);

    let event: AuditEventV1 = serde_json::from_value(fixture).expect("mirror reads the fixture");
    event.validate().expect("the fixture is a valid marker");
    for data in [
        serde_json::json!({"operation": "event_snapshot_shrunk", "seen": 2}),
        serde_json::json!({"operation": "event_snapshot_shrunk", "seen": 2, "len": 1, "x": 1}),
    ] {
        let mut drifted = event.clone();
        drifted.data = data;
        assert!(
            drifted.validate().is_err(),
            "the Shell mirror must reject a drifted truncation payload: {}",
            drifted.data
        );
    }
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn observe_shell_events_shrink_emits_a_warning() {
    struct SharedWriter(std::sync::Arc<std::sync::Mutex<Vec<u8>>>);
    impl std::io::Write for SharedWriter {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    let captured = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let writer_handle = captured.clone();
    let subscriber = tracing_subscriber::fmt()
        .with_writer(move || SharedWriter(writer_handle.clone()))
        .finish();
    let _guard = shrink_guard();
    let root = private_root();
    tracing::subscriber::with_default(subscriber, || {
        let mut recorder = recording_recorder(&root);
        let started = ShellEvent::command_started("session-1", "cmd-1", "ls -l", "/tmp/work", 1);
        let mut completed = started.clone();
        completed.kind = ShellEventKind::CommandCompleted;
        completed.exit_code = Some(0);
        recorder.observe_shell_events(&[started.clone(), completed]);
        recorder.observe_shell_events(&[started]);
    });

    let output = String::from_utf8(captured.lock().unwrap().clone()).unwrap();
    assert!(
        output.contains("shell event snapshot shrank"),
        "the skipped batch must leave a warning trail: {output}"
    );
    assert!(output.contains("cosh_audit"), "{output}");
    let _ = std::fs::remove_dir_all(root);
}

/// Counts persisted command records by event type and optional command id.
fn record_count(root: &Path, event_type: &str, command_id: Option<&str>) -> usize {
    walk_segment_text(root)
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| serde_json::from_str::<serde_json::Value>(line).expect("audit record"))
        .filter(|record| record["event_type"] == event_type)
        .filter(|record| {
            command_id.is_none_or(|command_id| record["identity"]["command_id"] == command_id)
        })
        .count()
}

fn recording_recorder(root: &Path) -> ShellAuditRecorder {
    ShellAuditRecorder {
        writer: Some(AuditSegmentWriter::create(root).unwrap()),
        writer_root: Some(root.to_path_buf()),
        mode: AuditMode::BestEffort,
        max_disk_bytes: crate::config::audit::AuditSettings::default().max_disk_bytes,
        shell_session_id: "session-1".to_string(),
        seen_events: 0,
        truncation: TruncationReport::Idle,
        hash_salt: "salt".to_string(),
        degraded: false,
        warning_emitted: false,
        owned_approvals: std::collections::HashSet::new(),
        resolved_approvals: std::collections::HashSet::new(),
        command_refs: std::collections::HashMap::new(),
    }
}

/// Returns the redaction claim of the first record with the given event type.
fn redaction_claim(root: &Path, event_type: &str) -> serde_json::Value {
    walk_segment_text(root)
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| serde_json::from_str::<serde_json::Value>(line).expect("audit record"))
        .find(|record| record["event_type"] == event_type)
        .unwrap_or_else(|| panic!("no {event_type} record was persisted"))["redaction"]
        .clone()
}

fn clean_claim() -> serde_json::Value {
    serde_json::json!({
        "policy_version": AUDIT_REDACTION_POLICY_VERSION,
        "status": "clean",
    })
}

fn dropped_claim(fields: &[&str]) -> serde_json::Value {
    serde_json::json!({
        "policy_version": AUDIT_REDACTION_POLICY_VERSION,
        "status": "dropped",
        "fields": fields,
    })
}

#[test]
fn command_events_claim_only_the_fields_the_projection_omits() {
    let root = private_root();
    let mut recorder = recording_recorder(&root);
    let started = ShellEvent::command_started("session-1", "cmd-1", "ls -l", "/tmp/work", 1);
    let mut completed = started.clone();
    completed.kind = ShellEventKind::CommandCompleted;
    completed.exit_code = Some(0);
    let mut failed = started.clone();
    failed.kind = ShellEventKind::CommandFailed;
    failed.exit_code = Some(1);
    recorder.observe_shell_events(&[started, completed, failed]);
    drop(recorder);

    for event_type in [
        "shell.command.started",
        "shell.command.completed",
        "shell.command.failed",
    ] {
        assert_eq!(
            redaction_claim(&root, event_type),
            dropped_claim(&["command", "cwd"]),
            "{event_type} must claim exactly the omitted source fields"
        );
    }
    assert_eq!(
        redaction_claim(&root, "session.ended"),
        clean_claim(),
        "the session lifecycle payload holds no source field to omit"
    );
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn command_without_a_cwd_does_not_claim_a_dropped_cwd() {
    let root = private_root();
    let mut recorder = recording_recorder(&root);
    let mut started = ShellEvent::command_started("session-1", "cmd-1", "ls -l", "/tmp/work", 1);
    started.cwd = None;
    recorder.observe_shell_events(&[started]);
    drop(recorder);

    assert_eq!(
        redaction_claim(&root, "shell.command.started"),
        dropped_claim(&["command"])
    );
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn command_with_a_secret_program_also_claims_the_program_field() {
    let root = private_root();
    let mut recorder = recording_recorder(&root);
    let started = ShellEvent::command_started(
        "session-1",
        "cmd-1",
        "ghp_0123456789abcdefghijklmnopqrstuvwxyz --help",
        "/tmp/work",
        1,
    );
    recorder.observe_shell_events(&[started]);
    drop(recorder);

    assert_eq!(
        redaction_claim(&root, "shell.command.started"),
        dropped_claim(&["command", "cwd", "program"]),
        "a scanner-rewritten program must be visible in the claim"
    );
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn approval_events_claim_only_the_hashed_preview() {
    let root = private_root();
    let mut recorder = recording_recorder(&root);
    let request = |status| ShellApprovalAuditInput {
        id: "approval-1",
        audit_ref: None,
        session_id: "session-1",
        run_id: "run-1",
        request_id: Some("request-1"),
        tool_use_id: None,
        subject: "shell command",
        risk: "medium",
        assessment: None,
        preview: "$ echo ok",
        status,
    };
    assert!(recorder
        .record_approval_requested(request("pending"))
        .is_some());
    assert!(recorder
        .record_approval_resolved(request("approved"))
        .unwrap()
        .is_some());
    drop(recorder);

    assert_eq!(
        redaction_claim(&root, "approval.requested"),
        dropped_claim(&["preview"])
    );
    assert_eq!(
        redaction_claim(&root, "approval.resolved"),
        clean_claim(),
        "the resolution payload carries only a decision label"
    );
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn preset_audit_ref_still_records_approval_resolved() {
    let root = private_root();
    let mut recorder = recording_recorder(&root);
    let request = |status, audit_ref| ShellApprovalAuditInput {
        id: "approval-1",
        audit_ref,
        session_id: "session-1",
        run_id: "run-1",
        request_id: Some("request-1"),
        tool_use_id: None,
        subject: "shell command",
        risk: "medium",
        assessment: None,
        preview: "$ echo ok",
        status,
    };
    assert_eq!(
        recorder.record_approval_requested(request("pending", Some("external-audit-ref"))),
        Some("external-audit-ref".to_string()),
        "the external audit_ref should be returned unchanged"
    );
    assert!(
        recorder
            .record_approval_resolved(request("approved", Some("external-audit-ref")))
            .unwrap()
            .is_some(),
        "a preset audit_ref must not prevent the resolved event from being written"
    );
    drop(recorder);

    let text = walk_segment_text(&root);
    assert!(
        text.contains("approval.resolved"),
        "approval.resolved should be present when audit_ref was preset; got: {text}"
    );
    assert!(
        !text.contains("approval.requested"),
        "approval.requested should not be duplicated when an external owner already wrote it"
    );
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn preset_audit_ref_allows_host_execution_boundary() {
    let root = private_root();
    let mut recorder = recording_recorder(&root);
    let request = |status, audit_ref| ShellApprovalAuditInput {
        id: "approval-1",
        audit_ref,
        session_id: "session-1",
        run_id: "run-1",
        request_id: Some("request-1"),
        tool_use_id: Some("tool-1"),
        subject: "run_shell_command",
        risk: "medium",
        assessment: None,
        preview: "$ echo ok",
        status,
    };
    assert_eq!(
        recorder.record_approval_requested(request("pending", Some("external-audit-ref"))),
        Some("external-audit-ref".to_string())
    );
    assert!(recorder
        .record_approval_resolved(request("approved", Some("external-audit-ref")))
        .unwrap()
        .is_some());
    recorder
        .authorize_host_execution(
            request("approved", Some("external-audit-ref")),
            "shell_foreground_handoff",
        )
        .unwrap();
    drop(recorder);

    let text = walk_segment_text(&root);
    assert!(
        text.contains("tool.execution.started"),
        "an externally owned provider tool must still get the host-execution boundary; got: {text}"
    );
    assert!(
        text.contains("approval.resolved"),
        "approval.resolved should be present; got: {text}"
    );
    assert!(
        !text.contains("approval.requested"),
        "approval.requested should not be duplicated; got: {text}"
    );
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn evidence_and_recovery_markers_claim_clean_redaction() {
    let root = private_root();
    let mut recorder = recording_recorder(&root);
    recorder.degraded = true;
    assert!(recorder
        .record_evidence_accessed("command_output", Some("small"), None, true)
        .is_some());
    drop(recorder);

    for event_type in ["evidence.accessed", "audit.degraded", "audit.recovered"] {
        assert_eq!(
            redaction_claim(&root, event_type),
            clean_claim(),
            "{event_type} carries no producer-omitted field"
        );
    }
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn host_execution_boundary_claims_clean_redaction() {
    let root = private_root();
    let mut recorder = recording_recorder(&root);
    recorder
        .authorize_host_execution(
            ShellApprovalAuditInput {
                id: "approval-1",
                audit_ref: Some("core-approval-event"),
                session_id: "session-1",
                run_id: "run-1",
                request_id: Some("request-1"),
                tool_use_id: Some("tool-1"),
                subject: "run_shell_command",
                risk: "medium",
                assessment: None,
                preview: "$ echo ok",
                status: "approved",
            },
            "shell_foreground_handoff",
        )
        .unwrap();
    drop(recorder);

    assert_eq!(
        redaction_claim(&root, "tool.execution.started"),
        clean_claim(),
        "the handoff boundary never receives raw Tool input"
    );
    let _ = std::fs::remove_dir_all(root);
}

fn walk_segment_text(root: &Path) -> String {
    let mut text = String::new();
    let segments = root.join("v1/segments");
    for date in std::fs::read_dir(segments).unwrap() {
        for file in std::fs::read_dir(date.unwrap().path()).unwrap() {
            text.push_str(&std::fs::read_to_string(file.unwrap().path()).unwrap());
        }
    }
    text
}

fn approval_input(status: &'static str) -> ShellApprovalAuditInput<'static> {
    ShellApprovalAuditInput {
        id: "approval-s02",
        audit_ref: None,
        session_id: "session-1",
        run_id: "run-1",
        request_id: Some("request-1"),
        tool_use_id: Some("tool-1"),
        subject: "run_shell_command",
        risk: "medium",
        assessment: None,
        preview: "$ echo ok",
        status,
    }
}

/// S02: a *real* audit write failure must fail the governed boundary closed and
/// then recover, without recreating the recorder.
///
/// The failure is injected by making an ancestor of the audit root a regular
/// file, so `openat(O_DIRECTORY)` fails with ENOTDIR. Unlike `chmod 000`, this
/// is not a DAC check, so running the suite as root cannot bypass it.
#[test]
fn required_mode_real_write_failure_blocks_then_recovers() {
    let base = private_root();
    let blocker = base.join("blocker");
    std::fs::write(&blocker, b"not a directory").unwrap();
    let audit_root = blocker.join("audit");

    assert!(
        AuditSegmentWriter::create(&audit_root).is_err(),
        "precondition: the audit root must be genuinely unusable"
    );

    let mut recorder = ShellAuditRecorder {
        writer: AuditSegmentWriter::create(&audit_root).ok(),
        writer_root: Some(audit_root.clone()),
        mode: AuditMode::Required,
        max_disk_bytes: crate::config::audit::AuditSettings::default().max_disk_bytes,
        shell_session_id: "session-1".to_string(),
        seen_events: 0,
        truncation: TruncationReport::Idle,
        hash_salt: "salt".to_string(),
        degraded: true,
        warning_emitted: false,
        owned_approvals: std::collections::HashSet::new(),
        resolved_approvals: std::collections::HashSet::new(),
        command_refs: std::collections::HashMap::new(),
    };
    assert!(recorder.writer.is_none());

    // Registering the request is an optional record: it degrades to None
    // instead of erroring, and still marks Shell ownership so the governed
    // resolution below cannot be skipped as externally owned.
    assert!(recorder
        .record_approval_requested(approval_input("pending"))
        .is_none());

    let blocked = recorder.record_approval_resolved(approval_input("approved"));
    assert!(
        blocked
            .unwrap_err()
            .starts_with("AUDIT_REQUIRED_UNAVAILABLE"),
        "required audit must fail closed on a real write failure"
    );

    // Positive control after recovery: the same recorder dispatches again once
    // the root is usable, so the block is caused by the write failure alone.
    // The replacement directory must satisfy the writer's private-directory
    // rule (0700, owned by us), which is the same check that made the unusable
    // root fail.
    std::fs::remove_file(&blocker).unwrap();
    std::fs::create_dir_all(&audit_root).unwrap();
    std::fs::set_permissions(&audit_root, std::fs::Permissions::from_mode(0o700)).unwrap();
    let recovered = recorder
        .record_approval_resolved(approval_input("approved"))
        .expect("recorder must recover once the audit root is usable");
    assert!(
        recovered.is_some(),
        "recovered resolution must yield an event id"
    );

    let _ = std::fs::remove_dir_all(&base);
}
