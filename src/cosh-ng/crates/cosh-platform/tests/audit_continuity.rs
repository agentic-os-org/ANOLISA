//! Same-uid tampering of audit segments must surface sequence continuity
//! diagnostics, and status must declare the local integrity boundary.
//! Cryptographic tamper evidence remains a v1 non-goal.
#![cfg(unix)]

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use chrono::Utc;
use cosh_platform::audit::config::AuditRootSource;
use cosh_platform::audit::query::{audit_status, query_events, AuditEventFilter};
use cosh_platform::audit::reader::{read_all, AuditDiagnosticKind, AuditSchemaGeneration};
use cosh_platform::audit::store::{AuditDurability, AuditSegmentWriter};
use cosh_types::audit::{
    AuditActor, AuditActorKind, AuditComponent, AuditComponentName, AuditDecisionData,
    AuditEventOutcome, AuditEventType, AuditEventV1, AuditIdentity, AuditOutcomeStatus,
    AuditRedaction, AuditRedactionStatus, AuditSettings, AuditSubject, KnownAuditEventType,
};

fn private_root() -> (tempfile::TempDir, PathBuf) {
    let directory = tempfile::tempdir().unwrap();
    std::fs::set_permissions(directory.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let path = directory.path().canonicalize().unwrap();
    (directory, path)
}

fn decision_event(label: &str) -> AuditEventV1 {
    let now = Utc::now();
    AuditEventV1::new(
        uuid::Uuid::new_v4().to_string(),
        AuditEventType::from(KnownAuditEventType::PolicyDecision),
        now,
        now,
        0,
        AuditComponent {
            name: AuditComponentName::CoshShell,
            version: "0.29.1".to_string(),
        },
        AuditIdentity {
            shell_session_id: Some("continuity-session".to_string()),
            ..AuditIdentity::default()
        },
        AuditActor {
            kind: AuditActorKind::User,
            uid: Some(1000),
            euid: Some(1000),
        },
        AuditEventOutcome {
            status: AuditOutcomeStatus::Success,
            code: None,
            retryable: false,
        },
        AuditSubject {
            kind: "policy_action".to_string(),
            name: Some(label.to_string()),
        },
        &AuditDecisionData {
            decision: "allow".to_string(),
            reason_code: Some("default_policy".to_string()),
            policy_version: Some("continuity-v1".to_string()),
            duration_ms: None,
        },
        AuditRedaction {
            policy_version: "audit-redaction-v1".to_string(),
            status: AuditRedactionStatus::Clean,
            fields: Vec::new(),
        },
    )
    .unwrap()
}

fn write_segment(events: &mut [AuditEventV1]) -> (tempfile::TempDir, PathBuf, PathBuf) {
    let (directory, root) = private_root();
    let mut writer = AuditSegmentWriter::create(&root, AuditComponentName::CoshShell).unwrap();
    for event in events.iter_mut() {
        writer
            .append(event, AuditDurability::SecurityBoundary)
            .unwrap();
    }
    writer.close().unwrap();
    let path = closed_segment(&root);
    (directory, root, path)
}

fn closed_segment(root: &Path) -> PathBuf {
    let segments = root.join("v1/segments");
    let mut closed = Vec::new();
    for date in std::fs::read_dir(&segments).unwrap() {
        for entry in std::fs::read_dir(date.unwrap().path()).unwrap() {
            let path = entry.unwrap().path();
            if path.extension().and_then(|ext| ext.to_str()) == Some("jsonl") {
                closed.push(path);
            }
        }
    }
    assert_eq!(closed.len(), 1);
    closed.pop().unwrap()
}

fn active_segment(root: &Path) -> PathBuf {
    let segments = root.join("v1/segments");
    let mut active = Vec::new();
    for date in std::fs::read_dir(&segments).unwrap() {
        for entry in std::fs::read_dir(date.unwrap().path()).unwrap() {
            let path = entry.unwrap().path();
            if path.to_string_lossy().ends_with(".jsonl.active") {
                active.push(path);
            }
        }
    }
    assert_eq!(active.len(), 1);
    active.pop().unwrap()
}

fn lines(path: &Path) -> Vec<String> {
    std::fs::read_to_string(path)
        .unwrap()
        .lines()
        .map(String::from)
        .collect()
}

fn write_lines(path: &Path, kept: &[String]) {
    let mut bytes = kept.join("\n");
    bytes.push('\n');
    std::fs::write(path, bytes).unwrap();
}

fn three_events() -> Vec<AuditEventV1> {
    (0..3)
        .map(|index| decision_event(&format!("event-{index}")))
        .collect()
}

#[test]
fn deleted_valid_event_surfaces_sequence_gap() {
    let mut events = three_events();
    let (_directory, root, path) = write_segment(&mut events);
    let original = lines(&path);
    write_lines(&path, &[original[0].clone(), original[2].clone()]);

    let result = read_all(&root, false).unwrap();

    assert_eq!(result.events.len(), 2);
    assert_eq!(result.diagnostics.len(), 1);
    assert_eq!(result.diagnostics[0].kind, AuditDiagnosticKind::SequenceGap);
    assert_eq!(result.diagnostics[0].line, Some(2));
}

#[test]
fn forged_contiguous_append_is_accepted_without_tamper_evidence() {
    let mut events: Vec<_> = (0..2)
        .map(|index| decision_event(&format!("event-{index}")))
        .collect();
    let (_directory, root, path) = write_segment(&mut events);
    let mut forged = decision_event("forged");
    forged.assign_writer_fields(2, Utc::now());
    let mut bytes = std::fs::read(&path).unwrap();
    bytes.extend_from_slice(&serde_json::to_vec(&forged).unwrap());
    bytes.push(b'\n');
    std::fs::write(&path, bytes).unwrap();

    let result = read_all(&root, false).unwrap();

    // Declared boundary: without cryptographic evidence a schema-valid forgery
    // with a contiguous sequence is indistinguishable from a genuine record.
    assert_eq!(result.events.len(), 3);
    assert!(result.diagnostics.is_empty());
    assert!(result
        .events
        .iter()
        .any(|stored| stored.event.subject.name.as_deref() == Some("forged")));
}

#[test]
fn dense_segment_has_no_continuity_diagnostics() {
    let mut events = three_events();
    let (_directory, root, _path) = write_segment(&mut events);

    let result = read_all(&root, false).unwrap();

    assert_eq!(result.events.len(), 3);
    assert!(result.diagnostics.is_empty());
}

#[test]
fn head_deletion_surfaces_sequence_gap() {
    let mut events = three_events();
    let (_directory, root, path) = write_segment(&mut events);
    let original = lines(&path);
    write_lines(&path, &original[1..]);

    let result = read_all(&root, false).unwrap();

    assert_eq!(result.events.len(), 2);
    assert_eq!(result.diagnostics.len(), 1);
    assert_eq!(result.diagnostics[0].kind, AuditDiagnosticKind::SequenceGap);
    assert_eq!(result.diagnostics[0].line, Some(1));
}

#[test]
fn tail_truncation_remains_within_declared_boundary() {
    let mut events = three_events();
    let (_directory, root, path) = write_segment(&mut events);
    let original = lines(&path);
    write_lines(&path, &original[..2]);

    let result = read_all(&root, false).unwrap();

    // A dense prefix carries no trailing anchor; truncation of the last record
    // stays undetectable without cryptographic evidence (declared non-goal).
    assert_eq!(result.events.len(), 2);
    assert!(result.diagnostics.is_empty());
}

#[test]
fn duplicated_record_surfaces_sequence_gap() {
    let mut events = three_events();
    let (_directory, root, path) = write_segment(&mut events);
    let original = lines(&path);
    write_lines(
        &path,
        &[
            original[0].clone(),
            original[1].clone(),
            original[1].clone(),
            original[2].clone(),
        ],
    );

    let result = read_all(&root, false).unwrap();

    assert_eq!(result.events.len(), 4);
    assert_eq!(result.diagnostics.len(), 1);
    assert_eq!(result.diagnostics[0].kind, AuditDiagnosticKind::SequenceGap);
    assert_eq!(result.diagnostics[0].line, Some(3));
}

#[test]
fn reordered_records_surface_sequence_gap() {
    let mut events = three_events();
    let (_directory, root, path) = write_segment(&mut events);
    let original = lines(&path);
    write_lines(
        &path,
        &[
            original[1].clone(),
            original[0].clone(),
            original[2].clone(),
        ],
    );

    let result = read_all(&root, false).unwrap();

    assert_eq!(result.events.len(), 3);
    assert_eq!(result.diagnostics.len(), 3);
    let gaps: Vec<_> = result
        .diagnostics
        .iter()
        .map(|item| (item.kind, item.line))
        .collect();
    assert_eq!(
        gaps,
        vec![
            (AuditDiagnosticKind::SequenceGap, Some(1)),
            (AuditDiagnosticKind::SequenceGap, Some(2)),
            (AuditDiagnosticKind::SequenceGap, Some(3)),
        ]
    );
}

#[test]
fn two_segments_restart_sequences_without_gaps() {
    let (_directory, root) = private_root();
    for label in ["first", "second"] {
        let mut writer = AuditSegmentWriter::create(&root, AuditComponentName::CoshShell).unwrap();
        let mut event = decision_event(label);
        writer
            .append(&mut event, AuditDurability::SecurityBoundary)
            .unwrap();
        writer.close().unwrap();
    }

    let result = read_all(&root, false).unwrap();

    assert_eq!(result.events.len(), 2);
    assert!(result.diagnostics.is_empty());
}

#[test]
fn partial_tail_reports_only_trailing_partial_record() {
    let mut events = three_events();
    let (_directory, root, path) = write_segment(&mut events);
    let mut bytes = std::fs::read(&path).unwrap();
    bytes.extend_from_slice(b"{\"schema\":\"cosh.audit");
    std::fs::write(&path, bytes).unwrap();

    let result = read_all(&root, false).unwrap();

    assert_eq!(result.events.len(), 3);
    assert_eq!(result.diagnostics.len(), 1);
    assert_eq!(
        result.diagnostics[0].kind,
        AuditDiagnosticKind::TrailingPartialRecord
    );
}

#[test]
fn corrupted_interior_record_reports_invalid_json_and_gap() {
    let mut events = three_events();
    let (_directory, root, path) = write_segment(&mut events);
    let original = lines(&path);
    write_lines(
        &path,
        &[
            original[0].clone(),
            "not-json".to_string(),
            original[2].clone(),
        ],
    );

    let result = read_all(&root, false).unwrap();

    assert_eq!(result.events.len(), 2);
    let kinds: Vec<_> = result.diagnostics.iter().map(|item| item.kind).collect();
    assert_eq!(
        kinds,
        vec![
            AuditDiagnosticKind::InvalidJson,
            AuditDiagnosticKind::SequenceGap
        ]
    );
}

#[test]
fn active_segment_gap_is_diagnosed() {
    let (_directory, root) = private_root();
    let mut writer = AuditSegmentWriter::create(&root, AuditComponentName::CoshShell).unwrap();
    let mut events = three_events();
    for event in &mut events {
        writer
            .append(event, AuditDurability::SecurityBoundary)
            .unwrap();
    }
    let path = active_segment(&root);
    let original = lines(&path);
    write_lines(&path, &[original[0].clone(), original[2].clone()]);

    let result = read_all(&root, false).unwrap();

    assert_eq!(result.active_segments, 1);
    assert_eq!(result.events.len(), 2);
    assert_eq!(result.diagnostics.len(), 1);
    assert_eq!(result.diagnostics[0].kind, AuditDiagnosticKind::SequenceGap);
    assert_eq!(result.diagnostics[0].line, Some(2));
}

#[test]
fn legacy_inclusion_does_not_mask_v1_gap() {
    let mut events = three_events();
    let (_directory, root, path) = write_segment(&mut events);
    let original = lines(&path);
    write_lines(&path, &[original[0].clone(), original[2].clone()]);

    let result = read_all(&root, true).unwrap();

    let v1_events = result
        .events
        .iter()
        .filter(|stored| stored.generation == AuditSchemaGeneration::V1)
        .count();
    assert_eq!(v1_events, 2);
    assert!(result
        .diagnostics
        .iter()
        .any(|item| item.kind == AuditDiagnosticKind::SequenceGap));
}

#[test]
fn status_reports_integrity_boundary() {
    let mut events = three_events();
    let (_directory, root, _path) = write_segment(&mut events);

    let status = audit_status(
        &root,
        AuditRootSource::Environment,
        AuditSettings::default(),
    )
    .unwrap();

    assert!(!status.integrity_boundary.cryptographic_tamper_evidence);
    assert!(status.integrity_boundary.sequence_continuity_check);
    assert!(!status.integrity_boundary.non_repudiation);
    assert!(status.integrity_boundary.note.contains("non-repudiation"));
}

#[test]
fn events_page_carries_sequence_gap_diagnostic() {
    let mut events = three_events();
    let (_directory, root, path) = write_segment(&mut events);
    let original = lines(&path);
    write_lines(&path, &[original[0].clone(), original[2].clone()]);

    let page = query_events(&root, AuditEventFilter::default(), 100, None).unwrap();

    assert_eq!(page.events.len(), 2);
    assert_eq!(page.diagnostics.len(), 1);
    assert_eq!(page.diagnostics[0].kind, AuditDiagnosticKind::SequenceGap);
}
