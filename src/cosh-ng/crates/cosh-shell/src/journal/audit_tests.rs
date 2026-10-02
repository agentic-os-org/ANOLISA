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
        shell_session_id: "session-1".to_string(),
        seen_events: 0,
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
        shell_session_id: "session-1".to_string(),
        seen_events: 0,
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
        shell_session_id: "session-1".to_string(),
        seen_events: 0,
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
        shell_session_id: "session-1".to_string(),
        seen_events: 0,
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
        shell_session_id: "session-1".to_string(),
        seen_events: 0,
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
        shell_session_id: "session-1".to_string(),
        seen_events: 0,
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

fn recording_recorder(root: &Path) -> ShellAuditRecorder {
    ShellAuditRecorder {
        writer: Some(AuditSegmentWriter::create(root).unwrap()),
        writer_root: Some(root.to_path_buf()),
        mode: AuditMode::BestEffort,
        shell_session_id: "session-1".to_string(),
        seen_events: 0,
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
